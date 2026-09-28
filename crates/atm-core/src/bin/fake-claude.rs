//! Test double of the Claude CLI (spec §12.1); never bundled, never calls any API.
//! M1 stub: only `--version`. Owner: M2-CLAUDE (auth status, `-p` scenarios).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-v") => {
            println!("{} (Claude Code)", atm_types::CLAUDE_TESTED_VERSION)
        }
        _ => {
            eprintln!("fake-claude: {args:?} not implemented yet (M2-CLAUDE)");
            std::process::exit(2);
        }
    }
}
