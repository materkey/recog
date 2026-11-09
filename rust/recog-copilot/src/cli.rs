//! What the two binaries share on their command line: argparse's reading of options
//! that take one value, and the checkout they run for.

use std::path::PathBuf;

/// Reads `argv` as argparse reads options that each take one value (`--flag value` or
/// `--flag=value`) among `flags`, plus `-h`/`--help`. Each value goes to `apply` in
/// order; false when help was asked for.
///
/// # Errors
/// argparse's message for an unknown argument or a flag without its value, or what
/// `apply` refused.
pub fn parse_options(
    mut argv: impl Iterator<Item = String>,
    flags: &[&str],
    mut apply: impl FnMut(&str, String) -> Result<(), String>,
) -> Result<bool, String> {
    while let Some(arg) = argv.next() {
        let (flag, inline) = split_inline(&arg);
        if flag == "-h" || flag == "--help" {
            return Ok(false);
        }
        if !flags.contains(&flag) {
            return Err(format!("unrecognized arguments: {arg}"));
        }
        let value = inline
            .map(str::to_owned)
            .or_else(|| argv.next())
            .ok_or_else(|| format!("argument {flag}: expected one argument"))?;
        apply(flag, value)?;
    }
    Ok(true)
}

/// `--flag=value` as the flag and its value; anything else is all flag.
fn split_inline(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
        _ => (arg, None),
    }
}

/// The checkout: named, else the one this binary is installed in, else the working
/// directory (the service units set it).
///
/// # Errors
/// None named or installed, and no working directory.
pub fn repo_root(named: Option<PathBuf>) -> Result<PathBuf, String> {
    named
        .or_else(recog_core::install::installed_repo)
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|e| format!("no working directory: {e}"))
}
