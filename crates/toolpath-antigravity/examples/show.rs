//! Demo: derive a `ConversationView` from a real `agy` fixture and print
//! it. Run against one of the committed fixtures, e.g.:
//!
//! ```sh
//! cargo run -p toolpath-antigravity --example show -- \
//!   test-fixtures/antigravity/tool aa79cad9-c443-4070-9c84-f6df6d3920b2
//! ```

use toolpath_antigravity::reader::ConvoIO;
use toolpath_antigravity::{PathResolver, provider};
use toolpath_convo::{Item, Role};

fn main() {
    let mut args = std::env::args().skip(1);
    let gemini_dir = args
        .next()
        .expect("usage: show <path-to-.gemini-equivalent-dir> <conversation-id>");
    let conversation_id = args
        .next()
        .expect("usage: show <path-to-.gemini-equivalent-dir> <conversation-id>");

    let resolver = PathResolver::new().with_gemini_dir(&gemini_dir);
    let io = ConvoIO::with_resolver(resolver);
    let convo = io
        .read_conversation(&conversation_id)
        .unwrap_or_else(|e| panic!("reading {conversation_id} under {gemini_dir}: {e}"));
    let view = provider::to_view(&convo);

    println!("conversation {}", view.id);
    println!(
        "provider:     {}",
        view.provider_id.as_deref().unwrap_or("?")
    );
    if let Some(u) = &view.total_usage {
        println!(
            "tokens:       input={} output={}",
            u.input_tokens.unwrap_or(0),
            u.output_tokens.unwrap_or(0)
        );
    }
    println!();

    for item in &view.items {
        let Item::Turn(turn) = item else { continue };
        let who = match turn.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            Role::Other(ref s) => s.as_str(),
        };
        println!("[{who}] {}", turn.text);
        for tool in &turn.tool_uses {
            println!(
                "    tool_call: {} ({:?}) input={}",
                tool.name, tool.category, tool.input
            );
            if let Some(result) = &tool.result {
                let preview: String = result.content.chars().take(120).collect();
                println!("    tool_result: {preview}");
            }
        }
        println!();
    }
}
