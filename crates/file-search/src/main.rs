use std::io::IsTerminal;
use std::path::Path;

use clap::Parser;
use savfox_file_search::{Cli, FileMatch, Reporter, run_main};
use serde_json::json;

const WINDOWS_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;

fn main() -> anyhow::Result<()> {
    run_on_configured_stack(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_stack_size(WINDOWS_STACK_SIZE_BYTES)
            .build()?;
        runtime.block_on(async_main())
    })
}

async fn async_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let reporter = StdioReporter {
        write_output_as_json: cli.json,
        show_indices: cli.compute_indices && std::io::stdout().is_terminal(),
    };
    run_main(cli, reporter).await?;
    Ok(())
}

struct StdioReporter {
    write_output_as_json: bool,
    show_indices: bool,
}

impl Reporter for StdioReporter {
    fn report_match(&self, file_match: &FileMatch) {
        if self.write_output_as_json {
            println!("{}", serde_json::to_string(&file_match).unwrap());
        } else if self.show_indices {
            let indices = file_match
                .indices
                .as_ref()
                .expect("--compute-indices was specified");
            // `indices` is guaranteed to be sorted in ascending order. Instead
            // of calling `contains` for every character (which would be O(N^2)
            // in the worst-case), walk through the `indices` vector once while
            // iterating over the characters.
            let mut indices_iter = indices.iter().peekable();

            for (i, c) in file_match.path.to_string_lossy().chars().enumerate() {
                match indices_iter.peek() {
                    Some(next) if **next == i as u32 => {
                        // ANSI escape code for bold: \x1b[1m ... \x1b[0m
                        print!("\x1b[1m{c}\x1b[0m");
                        // advance the iterator since we've consumed this index
                        indices_iter.next();
                    }
                    _ => {
                        print!("{c}");
                    }
                }
            }
            println!();
        } else {
            println!("{}", file_match.path.to_string_lossy());
        }
    }

    fn warn_matches_truncated(&self, total_match_count: usize, shown_match_count: usize) {
        if self.write_output_as_json {
            let value = json!({"matches_truncated": true});
            println!("{}", serde_json::to_string(&value).unwrap());
        } else {
            eprintln!(
                "Warning: showing {shown_match_count} out of {total_match_count} results. Provide a more specific pattern or increase the --limit.",
            );
        }
    }

    fn warn_no_search_pattern(&self, search_directory: &Path) {
        eprintln!(
            "No search pattern specified. Showing the contents of the current directory ({}):",
            search_directory.to_string_lossy()
        );
    }
}

#[cfg(windows)]
fn run_on_configured_stack<F>(main_fn: F) -> anyhow::Result<()>
where
    F: FnOnce() -> anyhow::Result<()> + Send + 'static,
{
    let handle = std::thread::Builder::new()
        .name("savfox-file-search-main".to_owned())
        .stack_size(WINDOWS_STACK_SIZE_BYTES)
        .spawn(main_fn)?;

    match handle.join() {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[cfg(not(windows))]
fn run_on_configured_stack<F>(main_fn: F) -> anyhow::Result<()>
where
    F: FnOnce() -> anyhow::Result<()>,
{
    main_fn()
}
