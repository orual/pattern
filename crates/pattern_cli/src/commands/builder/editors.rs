//! Section editors using dialoguer for interactive input.

use std::fmt::Display;
use std::path::Path;

use dialoguer::{Confirm, Input, MultiSelect, Select};
use miette::Result;

/// Edit a simple text field.
///
/// Shows current value and prompts for new value.
/// Supports `@path` syntax for file references.
/// Returns None if user wants to keep current value.
pub fn edit_text(
    field_name: &str,
    current: Option<&str>,
    allow_empty: bool,
) -> Result<Option<String>> {
    let current_display = current.unwrap_or("(none)");
    let truncated = if current_display.len() > 60 {
        format!("{}...", &current_display[..60])
    } else {
        current_display.to_string()
    };

    println!(
        "\n{}: {}",
        field_name,
        owo_colors::OwoColorize::dimmed(&truncated)
    );
    println!("  (Use @ prefix to link a file, e.g., @./prompt.md)");

    let input: String = Input::new()
        .with_prompt("New value (empty to keep current)")
        .allow_empty(true)
        .interact_text()
        .map_err(|e| miette::miette!("Input error: {}", e))?;

    if input.is_empty() {
        return Ok(None); // Keep current
    }

    if !allow_empty && input.trim().is_empty() {
        return Err(miette::miette!("{} cannot be empty", field_name));
    }

    Ok(Some(input))
}

/// Edit a text field that can reference a file.
///
/// If the value starts with `@`, treats it as a file path and loads content.
/// Returns the resolved content (file contents or raw text).
pub fn edit_text_or_file(
    field_name: &str,
    current: Option<&str>,
    current_path: Option<&Path>,
) -> Result<TextOrPath> {
    let current_display = if let Some(path) = current_path {
        format!("(from file: {})", path.display())
    } else {
        current
            .map(|s| {
                if s.len() > 60 {
                    format!("{}...", &s[..60])
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_else(|| "(none)".to_string())
    };

    println!(
        "\n{}: {}",
        field_name,
        owo_colors::OwoColorize::dimmed(&current_display)
    );
    println!("  (Use @ prefix to link a file, e.g., @./prompt.md)");

    let input: String = Input::new()
        .with_prompt("New value (empty to keep current)")
        .allow_empty(true)
        .interact_text()
        .map_err(|e| miette::miette!("Input error: {}", e))?;

    if input.is_empty() {
        return Ok(TextOrPath::Keep);
    }

    if let Some(path_str) = input.strip_prefix('@') {
        let path = std::path::PathBuf::from(path_str.trim());
        Ok(TextOrPath::Path(path))
    } else {
        Ok(TextOrPath::Text(input))
    }
}

/// Result of editing a text-or-file field.
#[derive(Debug, Clone)]
pub enum TextOrPath {
    /// Keep the current value.
    Keep,
    /// Set to inline text.
    Text(String),
    /// Set to a file path (content will be loaded at save time).
    Path(std::path::PathBuf),
}

/// Edit an enum field with arrow-key selection.
pub fn edit_enum<T>(field_name: &str, options: &[T], current_index: usize) -> Result<usize>
where
    T: Display,
{
    let items: Vec<String> = options.iter().map(|o| o.to_string()).collect();

    println!("\n{}:", field_name);

    let selection = Select::new()
        .items(&items)
        .default(current_index)
        .interact()
        .map_err(|e| miette::miette!("Selection error: {}", e))?;

    Ok(selection)
}

/// Edit a list with multi-select checkboxes.
///
/// Returns indices of selected items.
pub fn edit_multiselect<T>(
    field_name: &str,
    options: &[T],
    currently_selected: &[usize],
) -> Result<Vec<usize>>
where
    T: Display,
{
    let items: Vec<String> = options.iter().map(|o| o.to_string()).collect();

    println!("\n{} (space to toggle, enter to confirm):", field_name);

    let selection = MultiSelect::new()
        .items(&items)
        .defaults(
            &(0..options.len())
                .map(|i| currently_selected.contains(&i))
                .collect::<Vec<_>>(),
        )
        .interact()
        .map_err(|e| miette::miette!("Selection error: {}", e))?;

    Ok(selection)
}

/// Known tools that can be selected.
pub const KNOWN_TOOLS: &[&str] = &[
    "block",
    "block_edit",
    "recall",
    "search",
    "send_message",
    "source",
    "web",
    "calculator",
    "file",
    "emergency_halt",
];

/// Edit tools list with multi-select plus custom tool option.
pub fn edit_tools_multiselect(current_tools: &[String]) -> Result<Vec<String>> {
    // Build display list: known tools + separator + add custom
    let mut display_items: Vec<String> = KNOWN_TOOLS.iter().map(|s| s.to_string()).collect();
    display_items.push("─────────────────".to_string());
    display_items.push("+ Add custom tool...".to_string());

    // Determine which known tools are currently selected
    let mut defaults: Vec<bool> = KNOWN_TOOLS
        .iter()
        .map(|t| current_tools.contains(&t.to_string()))
        .collect();
    defaults.push(false); // separator
    defaults.push(false); // add custom

    println!("\nSelect tools (space to toggle, enter to confirm):");

    let selection = MultiSelect::new()
        .items(&display_items)
        .defaults(&defaults)
        .interact()
        .map_err(|e| miette::miette!("Selection error: {}", e))?;

    let mut selected_tools: Vec<String> = Vec::new();

    // Add selected known tools
    for idx in &selection {
        if *idx < KNOWN_TOOLS.len() {
            selected_tools.push(KNOWN_TOOLS[*idx].to_string());
        }
    }

    // Check if "add custom" was selected
    let add_custom_idx = display_items.len() - 1;
    if selection.contains(&add_custom_idx) {
        // Prompt for custom tool names
        loop {
            let custom: String = Input::new()
                .with_prompt("Custom tool name (empty to finish)")
                .allow_empty(true)
                .interact_text()
                .map_err(|e| miette::miette!("Input error: {}", e))?;

            if custom.is_empty() {
                break;
            }
            if !selected_tools.contains(&custom) {
                selected_tools.push(custom);
            }
        }
    }

    // Also preserve any existing custom tools that weren't in KNOWN_TOOLS
    for tool in current_tools {
        if !KNOWN_TOOLS.contains(&tool.as_str()) && !selected_tools.contains(tool) {
            // Ask if they want to keep this custom tool
            let keep = Confirm::new()
                .with_prompt(format!("Keep custom tool '{}'?", tool))
                .default(true)
                .interact()
                .map_err(|e| miette::miette!("Confirm error: {}", e))?;
            if keep {
                selected_tools.push(tool.clone());
            }
        }
    }

    Ok(selected_tools)
}

/// Trait for items that can be displayed in a collection editor.
pub trait CollectionItem: Clone {
    /// Short display string for list view.
    fn display_short(&self) -> String;

    /// Label/key for the item.
    fn label(&self) -> String;
}

/// Action chosen in the collection editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionAction {
    Add,
    Edit(usize),
    Remove(usize),
    EditAsToml,
    Done,
}

/// Edit a collection of items with CRUD operations.
pub fn edit_collection<T: CollectionItem>(
    field_name: &str,
    items: &[T],
) -> Result<CollectionAction> {
    println!("\n{}:", field_name);

    if items.is_empty() {
        println!("  (no items)");
    } else {
        for (i, item) in items.iter().enumerate() {
            println!("  {}. {}", i + 1, item.display_short());
        }
    }
    println!();

    // Build menu options
    let mut options = Vec::new();
    for (i, item) in items.iter().enumerate() {
        options.push(format!("Edit: {}", item.label()));
        // We'll handle remove via a submenu
        let _ = i; // suppress warning
    }
    options.push("[Add new]".to_string());
    options.push("[Edit as TOML]".to_string());
    options.push("[Done]".to_string());

    let selection = Select::new()
        .items(&options)
        .default(options.len() - 1) // Default to Done
        .interact()
        .map_err(|e| miette::miette!("Selection error: {}", e))?;

    let num_items = items.len();

    if selection < num_items {
        // Selected an existing item to edit
        // Ask what to do with it
        let item_options = ["Edit", "Remove", "Cancel"];
        let item_action = Select::new()
            .with_prompt(&format!("Action for '{}'", items[selection].label()))
            .items(&item_options)
            .default(0)
            .interact()
            .map_err(|e| miette::miette!("Selection error: {}", e))?;

        match item_action {
            0 => Ok(CollectionAction::Edit(selection)),
            1 => Ok(CollectionAction::Remove(selection)),
            _ => Ok(CollectionAction::Done),
        }
    } else if selection == num_items {
        Ok(CollectionAction::Add)
    } else if selection == num_items + 1 {
        Ok(CollectionAction::EditAsToml)
    } else {
        Ok(CollectionAction::Done)
    }
}

/// Confirm an action with the user.
pub fn confirm(prompt: &str, default: bool) -> Result<bool> {
    Confirm::new()
        .with_prompt(prompt)
        .default(default)
        .interact()
        .map_err(|e| miette::miette!("Confirm error: {}", e))
}

/// Prompt for a required text input.
pub fn input_required(prompt: &str) -> Result<String> {
    Input::new()
        .with_prompt(prompt)
        .interact_text()
        .map_err(|e| miette::miette!("Input error: {}", e))
}

/// Prompt for an optional text input.
pub fn input_optional(prompt: &str) -> Result<Option<String>> {
    let input: String = Input::new()
        .with_prompt(prompt)
        .allow_empty(true)
        .interact_text()
        .map_err(|e| miette::miette!("Input error: {}", e))?;

    if input.is_empty() {
        Ok(None)
    } else {
        Ok(Some(input))
    }
}

/// Select from a menu of options, returning the selected index.
pub fn select_menu<T: Display>(prompt: &str, options: &[T], default: usize) -> Result<usize> {
    let items: Vec<String> = options.iter().map(|o| o.to_string()).collect();

    Select::new()
        .with_prompt(prompt)
        .items(&items)
        .default(default)
        .interact()
        .map_err(|e| miette::miette!("Selection error: {}", e))
}
