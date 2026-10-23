// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Claude Code statusLine relay: runs on every status refresh, so no Tauri app at all.
    if std::env::args().any(|a| a == "--claude-statusline") {
        soul_lib::claude_statusline();
        std::process::exit(0);
    }
    // Coding agent hook relay (`coding_agents/relay.rs`): must never block the agent.
    if std::env::args().skip(1).any(|a| a == "--agent-hook") {
        soul_lib::coding_agents::relay::run_cli();
        std::process::exit(0);
    }
    soul_lib::run()
}
