//! Экспорт `schema/rchat.schema.json` из типов `rchat_ffi::schema`.
//!
//! ```text
//! cargo run -p rchat-ffi --bin export_schema
//! cargo run -p rchat-ffi --bin export_schema -- --check
//! cargo run -p rchat-ffi --bin export_schema -- path/to/schema.json
//! ```

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn default_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("schema/rchat.schema.json")
}

fn help() {
    println!(
        "usage: export_schema [--check] [PATH]\n\
         \n\
         Without PATH writes <workspace>/schema/rchat.schema.json.\n\
         With --check compares the generated schema with the file and exits\n\
         with status 1 when it is stale."
    );
}

fn main() -> ExitCode {
    let mut check = false;
    let mut path: Option<PathBuf> = None;
    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--check" => check = true,
            "-h" | "--help" => {
                help();
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("export_schema: unknown option {other}");
                help();
                return ExitCode::from(2);
            }
            other => {
                if path.is_some() {
                    eprintln!("export_schema: only one output path is allowed");
                    return ExitCode::from(2);
                }
                path = Some(PathBuf::from(other));
            }
        }
    }
    let path = path.unwrap_or_else(default_path);
    let generated = match rchat_ffi::schema::schema_json() {
        Ok(text) => text,
        Err(error) => {
            eprintln!("export_schema: cannot serialize schema: {error}");
            return ExitCode::FAILURE;
        }
    };

    if check {
        let actual = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("export_schema: cannot read {}: {error}", path.display());
                return ExitCode::FAILURE;
            }
        };
        if actual != generated {
            eprintln!(
                "export_schema: {} is out of date; run \
                 `cargo run -p rchat-ffi --bin export_schema`",
                path.display()
            );
            return ExitCode::FAILURE;
        }
        println!("schema is up to date: {}", path.display());
        return ExitCode::SUCCESS;
    }

    if let Some(parent) = path.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!("export_schema: cannot create {}: {error}", parent.display());
            return ExitCode::FAILURE;
        }
    }
    if let Err(error) = fs::write(&path, generated) {
        eprintln!("export_schema: cannot write {}: {error}", path.display());
        return ExitCode::FAILURE;
    }
    println!("wrote {}", path.display());
    ExitCode::SUCCESS
}
