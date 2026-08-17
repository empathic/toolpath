//! `path show <provider> --session …` — emits a markdown summary for a single
//! session. Designed to back fzf's `--preview` window during interactive
//! derive selection, but also runnable on its own to inspect a session.
//!
//! Internally this derives the session's Path and renders it via
//! `toolpath_md` at `summary` detail, which keeps the output to file-level
//! diffstats — short enough for a preview pane, rich enough to choose by.
#![cfg(not(target_os = "emscripten"))]

use crate::config::Config;
use crate::providers;
use anyhow::Result;
use clap::Subcommand;
use toolpath::v1::Graph;

#[derive(Subcommand, Debug)]
pub enum ShowSource {
    /// Show a Claude conversation as a markdown summary
    Claude {
        /// Project path (e.g., /Users/alex/myproject)
        #[arg(short, long)]
        project: String,

        /// Session id (the file stem of the JSONL)
        #[arg(short, long)]
        session: String,
    },
    /// Show a Gemini CLI conversation as a markdown summary
    Gemini {
        /// Project path (e.g., /Users/alex/myproject)
        #[arg(short, long)]
        project: String,

        /// Session UUID (the directory name under chats/)
        #[arg(short, long)]
        session: String,
    },
    /// Show a Codex CLI session as a markdown summary
    Codex {
        /// Session id, UUID, or filename stem
        #[arg(short, long)]
        session: String,

        /// Compatibility shim for the unified `path share` preview template; ignored.
        #[arg(long, hide = true)]
        project: Option<std::path::PathBuf>,
    },
    /// Show a GitHub Copilot CLI session as a markdown summary (preview)
    Copilot {
        /// Session id or unique prefix
        #[arg(short, long)]
        session: String,

        /// Compatibility shim for the unified `path share` preview template; ignored.
        #[arg(long, hide = true)]
        project: Option<std::path::PathBuf>,
    },
    /// Show an opencode session as a markdown summary
    Opencode {
        /// Session id (`ses_…`)
        #[arg(short, long)]
        session: String,

        /// Compatibility shim for the unified `path share` preview template; ignored.
        #[arg(long, hide = true)]
        project: Option<std::path::PathBuf>,
    },
    /// Show a Cursor composer as a markdown summary
    Cursor {
        /// Composer UUID
        #[arg(short, long)]
        session: String,

        /// Compatibility shim for the unified `path share` preview template; ignored.
        #[arg(long, hide = true)]
        project: Option<std::path::PathBuf>,
    },
    /// Show a Pi (pi.dev) session as a markdown summary
    Pi {
        /// Project path
        #[arg(short, long)]
        project: String,

        /// Session id
        #[arg(short, long)]
        session: String,

        /// Override the Pi sessions base directory (default: ~/.pi/agent/sessions)
        #[arg(long)]
        base: Option<std::path::PathBuf>,
    },
}

pub fn run(source: ShowSource, ansi: bool, config: &Config) -> Result<()> {
    let path = derive_one(source, config)?;
    let doc = Graph::from_path(path);
    let opts = toolpath_md::RenderOptions {
        detail: toolpath_md::Detail::Summary,
        front_matter: false,
    };
    let md = toolpath_md::render(&doc, &opts);
    if ansi {
        print!("{}", crate::term::markdown_to_ansi(&md));
    } else {
        print!("{md}");
    }
    Ok(())
}

fn derive_one(source: ShowSource, config: &Config) -> Result<toolpath::v1::Path> {
    match source {
        ShowSource::Claude { project, session } => {
            let manager = providers::claude_convo(config);
            let convo = manager
                .read_conversation(&project, &session)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            let cfg = toolpath_claude::derive::DeriveConfig {
                project_path: Some(project),
                include_thinking: false,
            };
            Ok(toolpath_claude::derive::derive_path(&convo, &cfg))
        }
        ShowSource::Gemini { project, session } => {
            let manager = providers::gemini_convo(config);
            let convo = manager
                .read_conversation(&project, &session)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            let cfg = toolpath_gemini::derive::DeriveConfig {
                project_path: Some(project),
                include_thinking: false,
            };
            Ok(toolpath_gemini::derive::derive_path(&convo, &cfg))
        }
        ShowSource::Codex {
            session,
            project: _,
        } => {
            let manager = providers::codex_convo(config);
            let s = manager
                .read_session(&session)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            let cfg = toolpath_codex::derive::DeriveConfig { project_path: None };
            Ok(toolpath_codex::derive::derive_path(&s, &cfg))
        }
        ShowSource::Copilot {
            session,
            project: _,
        } => {
            let manager = providers::copilot_convo(config);
            let s = manager
                .read_session(&session)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            let cfg = toolpath_copilot::derive::DeriveConfig { project_path: None };
            Ok(toolpath_copilot::derive::derive_path(&s, &cfg))
        }
        ShowSource::Opencode {
            session,
            project: _,
        } => {
            let manager = providers::opencode_convo(config);
            let s = manager
                .read_session(&session)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            let cfg = toolpath_opencode::derive::DeriveConfig::default();
            Ok(toolpath_opencode::derive::derive_path_with_resolver(
                &s,
                &cfg,
                manager.resolver(),
            ))
        }
        ShowSource::Cursor {
            session,
            project: _,
        } => {
            let manager = providers::cursor_convo(config);
            let s = manager
                .read_session(&session)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            let cfg = toolpath_cursor::DeriveConfig::default();
            Ok(toolpath_cursor::derive_path(&s, &cfg))
        }
        ShowSource::Pi {
            project,
            session,
            base,
        } => {
            let mut resolver = providers::pi_resolver(config);
            if let Some(p) = base {
                resolver = resolver.with_sessions_dir(&p);
            }
            let manager = toolpath_pi::PiConvo::with_resolver(resolver);
            let s = manager
                .read_session(&project, &session)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            let cfg = toolpath_pi::DeriveConfig::default();
            Ok(toolpath_pi::derive::derive_path(&s, &cfg))
        }
    }
}
