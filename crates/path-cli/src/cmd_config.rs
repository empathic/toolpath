//! `path config` — edit the user configuration.
//!
//! Today the only operation is `edit`: open `~/.toolpath/config.toml`
//! in `$VISUAL`/`$EDITOR` (creating it from a commented template first)
//! and validate the result, so a typo surfaces at edit time instead of
//! at the next `share`. Field-level get/set and `[[project]]` rule
//! management are meant to grow on top of this.

#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;

use crate::config::{CONFIG_FILE_NAME, config_dir, home_dir, home_relative};

#[derive(Subcommand, Debug)]
pub enum ConfigOp {
    /// Open the config file in $VISUAL/$EDITOR, creating it from a
    /// commented template if missing, and validate it afterwards
    Edit,
}

pub fn run(op: ConfigOp) -> Result<()> {
    match op {
        ConfigOp::Edit => edit(),
    }
}

/// Written on first `path config edit`. Comments only — a fresh file
/// behaves exactly like no file.
const TEMPLATE: &str = "# Toolpath user configuration.\n# https://toolpath.net/cli/\n";

fn edit() -> Result<()> {
    let path = config_dir()?.join(CONFIG_FILE_NAME);
    ensure_config_file(&path)?;

    let editor = resolve_editor(std::env::var_os("VISUAL"), std::env::var_os("EDITOR"));
    run_editor(&editor, &path)?;

    let display = home_relative(&path, home_dir().as_deref());
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {display} back after editing"))?;
    let rules = crate::share_config::validate_config_text(&text, &display).with_context(|| {
        format!("{display} was saved but does not validate; run `path config edit` to fix it")
    })?;
    let plural = if rules == 1 { "" } else { "s" };
    println!("{display}: {rules} project rule{plural}");
    Ok(())
}

/// Create `path` from the template if it doesn't exist yet. Uses
/// `create_new` so a concurrently created file is never clobbered.
fn ensure_config_file(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => file
            .write_all(TEMPLATE.as_bytes())
            .with_context(|| format!("write {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e).with_context(|| format!("create {}", path.display())),
    }
}

/// `$VISUAL` beats `$EDITOR` (the usual convention); unset, empty, and
/// whitespace-only values fall through to the platform default.
fn resolve_editor(visual: Option<OsString>, editor: Option<OsString>) -> String {
    for value in [visual, editor].into_iter().flatten() {
        let value = value.to_string_lossy();
        if !value.trim().is_empty() {
            return value.into_owned();
        }
    }
    if cfg!(windows) { "notepad" } else { "vi" }.to_string()
}

/// Run `editor` on `file` and wait for it. The editor value goes
/// through a shell so multi-word values like `code --wait` work,
/// matching git's treatment of `$EDITOR`.
fn run_editor(editor: &str, file: &Path) -> Result<()> {
    #[cfg(unix)]
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg(editor) // $0, so shell errors name the editor
        .arg(file)
        .status();
    #[cfg(windows)]
    let status = std::process::Command::new("cmd")
        .arg("/C")
        .arg(format!("{editor} \"{}\"", file.display()))
        .status();
    let status = status.with_context(|| format!("failed to launch editor `{editor}`"))?;
    if !status.success() {
        bail!("editor `{editor}` exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn editor_resolution_prefers_visual_then_editor_then_default() {
        assert_eq!(resolve_editor(os("code -w"), os("vim")), "code -w");
        assert_eq!(resolve_editor(None, os("vim")), "vim");
        let default = resolve_editor(None, None);
        assert_eq!(default, if cfg!(windows) { "notepad" } else { "vi" });
        // Empty and whitespace-only values are treated as unset.
        assert_eq!(resolve_editor(os(""), os("vim")), "vim");
        assert_eq!(resolve_editor(os("  "), os("")), default);
    }

    #[test]
    fn template_is_created_once_and_validates_as_empty_config() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("cfg/config.toml");
        ensure_config_file(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, TEMPLATE);
        assert_eq!(
            crate::share_config::validate_config_text(&text, "template").unwrap(),
            0
        );

        // A second call must not clobber user content.
        std::fs::write(&path, "[[project]]\ndir = \"/x\"\n").unwrap();
        ensure_config_file(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[[project]]\ndir = \"/x\"\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_editor_invokes_shell_command_on_the_file() {
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("config.toml");
        std::fs::write(&file, "# start\n").unwrap();
        // A multi-word "editor" that appends to its argument — exercises
        // both the shell wrapping and the file argument passing.
        run_editor("printf 'x = 1\\n' >>", &file).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "# start\nx = 1\n");
    }

    #[cfg(unix)]
    #[test]
    fn run_editor_reports_nonzero_exit() {
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("config.toml");
        std::fs::write(&file, "").unwrap();
        let err = run_editor("false", &file).unwrap_err();
        assert!(err.to_string().contains("false"), "{err:#}");
    }
}
