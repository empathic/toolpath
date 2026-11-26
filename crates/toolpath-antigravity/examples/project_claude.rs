//! Demo: project a Claude Code conversation into an `agy` store on disk,
//! so a real `agy --conversation <id>` can load it. Used by the live
//! run sheet's store-projector step.
//!
//! ```sh
//! cargo run -p toolpath-antigravity --example project_claude -- \
//!   test-fixtures/claude/convo.jsonl <out-.gemini-dir> <conversation-uuid> <cwd>
//! ```
//!
//! `<out-.gemini-dir>` is what `~/.gemini` would resolve to; never pass
//! the real one. `<cwd>` is the absolute working directory agy will be
//! started from (keys `cache/last_conversations.json`).

use toolpath_antigravity::project::AntigravityProjector;
use toolpath_convo::ConversationProjector;

fn main() {
    let usage = "usage: project_claude <claude.jsonl> <out-.gemini-dir> <conversation-uuid> <cwd>";
    let mut args = std::env::args().skip(1);
    let claude_path = args.next().expect(usage);
    let gemini_dir = args.next().expect(usage);
    let conversation_id = args.next().expect(usage);
    let cwd = args.next().expect(usage);

    let convo = toolpath_claude::ConversationReader::read_conversation(&claude_path)
        .unwrap_or_else(|e| panic!("reading {claude_path}: {e}"));
    let view = toolpath_claude::provider::to_view(&convo);

    let store = AntigravityProjector::new()
        .with_conversation_id(conversation_id.clone())
        .project(&view)
        .unwrap_or_else(|e| panic!("projecting {claude_path}: {e}"));
    store
        .write_to(std::path::Path::new(&gemini_dir), &cwd)
        .unwrap_or_else(|e| panic!("writing store under {gemini_dir}: {e}"));

    println!("wrote conversation {conversation_id} under {gemini_dir}");
    if !store.unmapped_tool_names.is_empty() {
        println!("unmapped tool names: {:?}", store.unmapped_tool_names);
    }
}
