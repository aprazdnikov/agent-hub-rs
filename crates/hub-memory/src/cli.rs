use std::{ffi::OsString, io, path::PathBuf};

use crate::{Store, serve};

/// Runs the local memory MCP server using arguments after `--memory-mcp`.
/// Errors contain diagnostics only, never argument values or memory contents.
pub fn run_cli(args: Vec<OsString>) -> Result<(), String> {
    let Options { project_dir, store_dir, actor } = Options::parse(args)?;
    let store = Store::open(&store_dir, &project_dir, &actor).map_err(|error| error.to_string())?;
    serve(store, io::stdin().lock(), io::stdout().lock())
        .map_err(|error| format!("memory MCP I/O failed ({:?})", error.kind()))
}

struct Options {
    project_dir: PathBuf,
    store_dir: PathBuf,
    actor: String,
}

impl Options {
    fn parse(args: Vec<OsString>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let mut project_dir = None;
        let mut store_dir = None;
        let mut actor = None;
        while let Some(option) = args.next() {
            let slot = match option.to_str() {
                Some("--project-dir") => &mut project_dir,
                Some("--store-dir") => &mut store_dir,
                Some("--actor") => &mut actor,
                Some(_) | None => return Err("unknown memory MCP option".into()),
            };
            if slot.is_some() {
                return Err("duplicate memory MCP option".into());
            }
            *slot = Some(args.next().ok_or("missing memory MCP option value")?);
        }
        let project_dir = PathBuf::from(project_dir.ok_or("missing --project-dir")?);
        if !project_dir.is_absolute() || !project_dir.is_dir() {
            return Err("--project-dir must be an absolute existing directory".into());
        }
        let store_dir = match store_dir {
            Some(path) => PathBuf::from(path),
            None => directories::BaseDirs::new()
                .ok_or("could not determine memory home directory")?
                .home_dir()
                .join(".agent-hub/memory"),
        };
        if !store_dir.is_absolute() {
            return Err("--store-dir must be an absolute directory".into());
        }
        let actor = actor
            .map_or_else(|| Ok("external".to_owned()), OsString::into_string)
            .map_err(|_| "--actor must be valid UTF-8")?;
        if actor.trim().is_empty() || actor.len() > 128 {
            return Err("--actor must contain 1 to 128 bytes".into());
        }
        Ok(Self { project_dir, store_dir, actor })
    }
}
