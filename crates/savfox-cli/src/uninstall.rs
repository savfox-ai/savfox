//! `savfox uninstall` — Clean removal of savfox data and configuration.

use std::path::PathBuf;

use clap::Parser;

/// Uninstall savfox — remove configuration and cached data
#[derive(Debug, Parser)]
pub struct UninstallCommand {
    /// Skip confirmation prompt
    #[clap(long)]
    pub force: bool,
    /// Also remove installed skills
    #[clap(long)]
    pub include_skills: bool,
    /// Only show what would be deleted (dry run)
    #[clap(long)]
    pub dry_run: bool,
}

pub async fn run(cmd: UninstallCommand) -> Result<(), Box<dyn std::error::Error>> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let savfox_dir = home.join(".savfox");

    if !savfox_dir.exists() {
        eprintln!(
            "Nothing to uninstall — {} does not exist",
            savfox_dir.display()
        );
        return Ok(());
    }

    eprintln!("The following will be removed:");
    eprintln!(
        "  {} (configuration, sessions, memory)",
        savfox_dir.display()
    );
    if cmd.include_skills {
        eprintln!(
            "  {} (installed skills)",
            savfox_dir.join("skills").display()
        );
    }

    if cmd.dry_run {
        eprintln!("\n(Dry run — no files were deleted)");
        return Ok(());
    }

    if !cmd.force {
        eprintln!("\nThis action is irreversible. Use --force to proceed.");
        return Ok(());
    }

    // Remove config directory
    if cmd.include_skills {
        // Remove everything
        match tokio::fs::remove_dir_all(&savfox_dir).await {
            Ok(()) => eprintln!("Removed {}", savfox_dir.display()),
            Err(e) => eprintln!("Error removing {}: {e}", savfox_dir.display()),
        }
    } else {
        // Remove everything except skills/
        let skills_dir = savfox_dir.join("skills");
        let mut entries = tokio::fs::read_dir(&savfox_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path == skills_dir {
                continue;
            }
            if path.is_dir() {
                let _ = tokio::fs::remove_dir_all(&path).await;
            } else {
                let _ = tokio::fs::remove_file(&path).await;
            }
            eprintln!("  Removed {}", path.display());
        }
    }

    eprintln!("\nSavfox has been uninstalled.");
    Ok(())
}
