//! `savfox docker` -- bootstrap Docker Compose files.

use std::path::PathBuf;

use clap::Parser;

const DOCKER_COMPOSE_TEMPLATE: &str = include_str!("../../../compose.yml");
const DOCKER_ENV_TEMPLATE: &str = include_str!("../../../.env.example");

#[derive(Debug, Parser)]
pub struct DockerCommand {
    #[clap(subcommand)]
    pub action: DockerAction,
}

#[derive(Debug, clap::Subcommand)]
pub enum DockerAction {
    /// Generate a production-ready docker-compose file for Savfox.
    Init {
        /// Output compose file path.
        #[clap(long, default_value = "docker-compose.yml")]
        output: PathBuf,
        /// Write an accompanying environment template file.
        #[clap(long, default_value_t = true)]
        with_env: bool,
        /// Overwrite existing files.
        #[clap(long)]
        force: bool,
    },
}

pub async fn run(cmd: DockerCommand) -> Result<(), Box<dyn std::error::Error>> {
    match cmd.action {
        DockerAction::Init {
            output,
            with_env,
            force,
        } => {
            write_file_if_allowed(&output, DOCKER_COMPOSE_TEMPLATE, force, "docker compose")?;
            println!("Wrote {}", output.display());

            if with_env {
                let env_output = output
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .join(".env.example");
                write_file_if_allowed(&env_output, DOCKER_ENV_TEMPLATE, force, "env template")?;
                println!("Wrote {}", env_output.display());
            }

            println!("Docker bootstrap complete.");
            println!("Next:");
            println!("  1. Edit .env.example (or copy to .env) and set SAVFOX_TOKEN");
            println!("  2. Run: docker compose up -d");
        }
    }
    Ok(())
}

fn write_file_if_allowed(
    path: &PathBuf,
    content: &str,
    force: bool,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() && !force {
        return Err(format!(
            "{label} file '{}' already exists (use --force to overwrite)",
            path.display()
        )
        .into());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}
