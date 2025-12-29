//! Save flow for the builder.

use std::path::PathBuf;

use dialoguer::{Input, Select};
use miette::Result;
use owo_colors::OwoColorize;

use super::{ConfigSource, SaveDestination};

/// Context for saving configuration.
pub struct SaveContext {
    /// Whether this is an update (vs new creation).
    #[allow(dead_code)]
    pub is_update: bool,
    /// Original source of the config.
    #[allow(dead_code)]
    pub source: ConfigSource,
    /// Default file path for export.
    pub default_path: Option<PathBuf>,
}

impl SaveContext {
    pub fn new(source: ConfigSource) -> Self {
        Self {
            is_update: matches!(source, ConfigSource::FromDb(_)),
            source,
            default_path: None,
        }
    }

    #[allow(dead_code)]
    pub fn with_default_path(mut self, path: PathBuf) -> Self {
        self.default_path = Some(path);
        self
    }
}

/// Prompt user for save destination.
pub fn prompt_save_destination() -> Result<SaveDestination> {
    let options = [
        SaveDestination::Database,
        SaveDestination::File,
        SaveDestination::Both,
        SaveDestination::Preview,
        SaveDestination::Cancel,
    ];

    let display: Vec<&str> = options.iter().map(|o| o.display_name()).collect();

    println!();
    let selection = Select::new()
        .with_prompt("Where to save?")
        .items(&display)
        .default(0)
        .interact()
        .map_err(|e| miette::miette!("Selection error: {}", e))?;

    Ok(options[selection])
}

/// Prompt for file path to export to.
pub fn prompt_file_path(default: Option<&PathBuf>, name: &str) -> Result<PathBuf> {
    let default_path = default
        .cloned()
        .unwrap_or_else(|| PathBuf::from(format!("./{}.toml", name)));

    let path_str: String = Input::new()
        .with_prompt("Export path")
        .default(default_path.display().to_string())
        .interact_text()
        .map_err(|e| miette::miette!("Input error: {}", e))?;

    Ok(PathBuf::from(path_str))
}

/// Show a TOML preview.
pub fn show_preview(toml_content: &str) {
    println!("\n{}", "─".repeat(60).dimmed());
    println!("{}", "TOML Preview:".bold());
    println!("{}", "─".repeat(60).dimmed());
    println!("{}", toml_content);
    println!("{}", "─".repeat(60).dimmed());
}

/// Save result with information about what was saved.
#[derive(Debug)]
pub struct SaveResult {
    pub saved_to_db: bool,
    pub saved_to_file: Option<PathBuf>,
    pub db_id: Option<String>,
}

impl SaveResult {
    pub fn display(&self) {
        if self.saved_to_db {
            if let Some(ref id) = self.db_id {
                println!("{} Saved to database (ID: {})", "✓".green(), id);
            } else {
                println!("{} Saved to database", "✓".green());
            }
        }
        if let Some(ref path) = self.saved_to_file {
            println!("{} Exported to {}", "✓".green(), path.display());
        }
    }
}

/// Generic save function that handles the save flow.
///
/// Takes closures for the actual save operations to keep this module
/// agnostic of the specific config types.
pub async fn save_config<F, G, Fut>(
    ctx: &SaveContext,
    name: &str,
    to_toml: F,
    save_to_db: G,
) -> Result<Option<SaveResult>>
where
    F: Fn() -> Result<String>,
    G: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    // Loop to handle Preview -> ask again flow
    loop {
        let destination = prompt_save_destination()?;

        match destination {
            SaveDestination::Cancel => {
                println!("{}", "Cancelled.".yellow());
                return Ok(None);
            }

            SaveDestination::Preview => {
                let toml = to_toml()?;
                show_preview(&toml);
                println!();
                // Loop continues, will prompt again
            }

            SaveDestination::Database => {
                let id = save_to_db().await?;
                return Ok(Some(SaveResult {
                    saved_to_db: true,
                    saved_to_file: None,
                    db_id: Some(id),
                }));
            }

            SaveDestination::File => {
                let path = prompt_file_path(ctx.default_path.as_ref(), name)?;
                let toml = to_toml()?;
                write_toml_file(&path, &toml)?;
                return Ok(Some(SaveResult {
                    saved_to_db: false,
                    saved_to_file: Some(path),
                    db_id: None,
                }));
            }

            SaveDestination::Both => {
                let path = prompt_file_path(ctx.default_path.as_ref(), name)?;
                let toml = to_toml()?;
                write_toml_file(&path, &toml)?;

                let id = save_to_db().await?;
                return Ok(Some(SaveResult {
                    saved_to_db: true,
                    saved_to_file: Some(path),
                    db_id: Some(id),
                }));
            }
        }
    }
}

/// Write TOML content to a file.
fn write_toml_file(path: &PathBuf, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| miette::miette!("Failed to create directory: {}", e))?;
        }
    }

    std::fs::write(path, content).map_err(|e| miette::miette!("Failed to write file: {}", e))?;

    Ok(())
}
