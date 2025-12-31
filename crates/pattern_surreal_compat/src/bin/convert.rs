//! CAR v1/v2 to v3 converter binary.
//!
//! Usage: car-convert [OPTIONS] <input.car> <output.car>

use std::path::PathBuf;
use std::process::ExitCode;

use pattern_surreal_compat::convert::{ConversionOptions, convert_car_v1v2_to_v3};

fn print_usage() {
    eprintln!("CAR v1/v2 to v3 converter");
    eprintln!();
    eprintln!("Usage: car-convert [OPTIONS] <input.car> <output.car>");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --keep-archival-blocks  Keep archival blocks as memory blocks");
    eprintln!("                          (default: convert to archival entries)");
    eprintln!();
    eprintln!("Converts legacy Pattern CAR exports (v1/v2 SurrealDB format)");
    eprintln!("to the current v3 format (SQLite-compatible).");
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 && (args[1] == "-h" || args[1] == "--help") {
        print_usage();
        return ExitCode::SUCCESS;
    }

    // Parse options and positional args
    let mut keep_archival_blocks = false;
    let mut positional: Vec<&str> = Vec::new();

    for arg in args.iter().skip(1) {
        if arg == "--keep-archival-blocks" {
            keep_archival_blocks = true;
        } else if arg.starts_with('-') {
            eprintln!("Unknown option: {}", arg);
            print_usage();
            return ExitCode::from(1);
        } else {
            positional.push(arg);
        }
    }

    if positional.len() != 2 {
        print_usage();
        return ExitCode::from(1);
    }

    let input_path = PathBuf::from(positional[0]);
    let output_path = PathBuf::from(positional[1]);

    if !input_path.exists() {
        eprintln!("Error: Input file does not exist: {}", input_path.display());
        return ExitCode::from(1);
    }

    if output_path.exists() {
        eprintln!(
            "Error: Output file already exists: {}",
            output_path.display()
        );
        eprintln!("Remove it first or choose a different output path.");
        return ExitCode::from(1);
    }

    let options = ConversionOptions {
        archival_blocks_to_entries: !keep_archival_blocks,
    };

    match convert_car_v1v2_to_v3(&input_path, &output_path, &options).await {
        Ok(stats) => {
            println!();
            println!("Conversion successful!");
            println!("  Input version: v{}", stats.input_version);
            println!("  Agents converted: {}", stats.agents_converted);
            println!("  Groups converted: {}", stats.groups_converted);
            println!("  Messages converted: {}", stats.messages_converted);
            println!(
                "  Memory blocks converted: {}",
                stats.memory_blocks_converted
            );
            println!(
                "  Archival entries converted: {}",
                stats.archival_entries_converted
            );
            println!();
            println!("Output written to: {}", output_path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Conversion failed: {}", e);
            ExitCode::from(1)
        }
    }
}
