use clap::Parser;
use savfox_arg0::arg0_dispatch_or_else;
use savfox_tui::{Cli, run_main};

#[derive(Parser, Debug)]
struct TopCli {
    #[clap(flatten)]
    inner: Cli,
}

fn print_boxed_summary(lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let inner_width = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let border = "─".repeat(inner_width + 2);
    println!("╭{border}╮");
    for line in lines {
        let padding = inner_width.saturating_sub(line.chars().count());
        println!("│ {line}{} │", " ".repeat(padding));
    }
    println!("╰{border}╯");
}

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|savfox_linux_sandbox_exe| async move {
        let cli = TopCli::parse().inner;
        let exit_info = run_main(cli, savfox_linux_sandbox_exe).await?;
        let token_usage = exit_info.token_usage;
        let session_id = exit_info.session_id;
        let model_display = exit_info.model_display;
        let directory = exit_info.directory;

        if let Some(id) = session_id {
            if !token_usage.is_zero() {
                println!("{}", savfox_core::protocol::FinalOutput::from(token_usage),);
            }

            let resume_command = savfox_core::util::resume_command(None, Some(id));

            let mut summary_lines: Vec<String> = Vec::new();
            if let Some(model) = model_display {
                summary_lines.push(format!("{:<11}{model}", "model:"));
            }
            if let Some(directory) = directory {
                summary_lines.push(format!("{:<11}{}", "directory:", directory.display()));
            }
            if let Some(cmd) = resume_command {
                summary_lines.push(format!("{:<11}{cmd}", "resume:"));
            }

            if !summary_lines.is_empty() {
                println!();
                print_boxed_summary(&summary_lines);
            }
        }
        Ok(())
    })
}
