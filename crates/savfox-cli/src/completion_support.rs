use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use clap::{CommandFactory, ValueEnum};
use clap_complete::{Shell, generate};
use savfox_core::config::find_savfox_home;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::MultitoolCli;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum DynamicCompletionKind {
    Models,
    Agents,
    Sessions,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CompletionCache {
    source_fingerprints: BTreeMap<String, u64>,
    models: Vec<String>,
    agents: Vec<String>,
    sessions: Vec<String>,
}

pub(crate) fn detect_shell_from_env() -> Shell {
    if cfg!(windows) {
        return Shell::PowerShell;
    }

    let shell = std::env::var("SHELL")
        .ok()
        .unwrap_or_default()
        .to_lowercase();

    if shell.contains("zsh") {
        Shell::Zsh
    } else if shell.contains("fish") {
        Shell::Fish
    } else if shell.contains("pwsh") || shell.contains("powershell") {
        Shell::PowerShell
    } else {
        Shell::Bash
    }
}

pub(crate) fn output_completion(shell: Shell, install: bool) -> Result<Option<PathBuf>> {
    let script = render_completion_script(shell)?;
    if install {
        let path = install_completion_script(shell, &script)?;
        eprintln!("installed {shell:?} completion: {}", path.display());
        Ok(Some(path))
    } else {
        print!("{script}");
        Ok(None)
    }
}

pub(crate) fn install_for_current_shell() -> Result<PathBuf> {
    let shell = detect_shell_from_env();
    let script = render_completion_script(shell)?;
    install_completion_script(shell, &script)
}

pub(crate) fn print_dynamic_values(kind: DynamicCompletionKind, refresh_cache: bool) -> Result<()> {
    let savfox_home = resolve_savfox_home()?;
    let cache = load_or_refresh_completion_cache(&savfox_home, refresh_cache)?;
    let values = match kind {
        DynamicCompletionKind::Models => cache.models,
        DynamicCompletionKind::Agents => cache.agents,
        DynamicCompletionKind::Sessions => cache.sessions,
    };
    for value in values {
        println!("{value}");
    }
    Ok(())
}

fn resolve_savfox_home() -> Result<PathBuf> {
    if let Ok(path) = find_savfox_home() {
        return Ok(path);
    }
    let home = dirs::home_dir().context("failed to resolve home directory for completions")?;
    Ok(home.join(".savfox"))
}

fn render_completion_script(shell: Shell) -> Result<String> {
    let mut cmd = MultitoolCli::command();
    let mut bytes = Vec::new();
    generate(shell, &mut cmd, "savfox", &mut bytes);
    let mut script = String::from_utf8(bytes).context("completion output is not valid UTF-8")?;
    script.push_str(match shell {
        Shell::Bash => BASH_DYNAMIC_SNIPPET,
        Shell::Zsh => ZSH_DYNAMIC_SNIPPET,
        Shell::Fish => FISH_DYNAMIC_SNIPPET,
        Shell::PowerShell => POWERSHELL_DYNAMIC_SNIPPET,
        _ => "",
    });
    Ok(script)
}

fn install_completion_script(shell: Shell, script: &str) -> Result<PathBuf> {
    let home =
        dirs::home_dir().context("failed to resolve home directory for completion install")?;
    match shell {
        Shell::Bash => {
            let path = home.join(".local/share/bash-completion/completions/savfox");
            write_completion_file(&path, script)?;
            Ok(path)
        }
        Shell::Zsh => {
            let path = home.join(".zsh/completions/_savfox");
            write_completion_file(&path, script)?;
            Ok(path)
        }
        Shell::Fish => {
            let path = home.join(".config/fish/completions/savfox.fish");
            write_completion_file(&path, script)?;
            Ok(path)
        }
        Shell::PowerShell => {
            let profile_path = home.join("Documents/PowerShell/Microsoft.PowerShell_profile.ps1");
            install_into_powershell_profile(&profile_path, script)?;
            Ok(profile_path)
        }
        _ => anyhow::bail!("automatic install for {shell:?} is not supported"),
    }
}

fn write_completion_file(path: &Path, script: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create completion dir {}", parent.display()))?;
    }
    fs::write(path, script)
        .with_context(|| format!("failed to write completion file {}", path.display()))?;
    Ok(())
}

fn install_into_powershell_profile(profile_path: &Path, script: &str) -> Result<()> {
    const START_MARKER: &str = "# >>> savfox completion >>>";
    const END_MARKER: &str = "# <<< savfox completion <<<";

    if let Some(parent) = profile_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create profile dir {}", parent.display()))?;
    }

    let existing = fs::read_to_string(profile_path).unwrap_or_default();
    let block = format!("{START_MARKER}\n{script}\n{END_MARKER}\n");
    let updated = if let Some(start) = existing.find(START_MARKER) {
        if let Some(rel_end) = existing[start..].find(END_MARKER) {
            let end = start + rel_end + END_MARKER.len();
            let mut out = String::new();
            out.push_str(existing[..start].trim_end());
            out.push('\n');
            out.push_str(&block);
            out.push_str(existing[end..].trim_start_matches(['\r', '\n']));
            out
        } else {
            format!("{}\n{block}", existing.trim_end())
        }
    } else if existing.trim().is_empty() {
        block
    } else {
        format!("{}\n\n{block}", existing.trim_end())
    };

    fs::write(profile_path, updated)
        .with_context(|| format!("failed to update {}", profile_path.display()))?;
    Ok(())
}

fn completion_cache_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("completion-cache.json")
}

fn load_or_refresh_completion_cache(
    savfox_home: &Path,
    force_refresh: bool,
) -> Result<CompletionCache> {
    let source_fingerprints = source_fingerprints(savfox_home);
    let cache_path = completion_cache_path(savfox_home);

    if !force_refresh {
        if let Ok(raw) = fs::read_to_string(&cache_path) {
            if let Ok(cache) = serde_json::from_str::<CompletionCache>(&raw) {
                if cache.source_fingerprints == source_fingerprints {
                    return Ok(cache);
                }
            }
        }
    }

    let cache = CompletionCache {
        source_fingerprints,
        models: collect_models(savfox_home),
        agents: collect_agents(savfox_home),
        sessions: collect_sessions(savfox_home),
    };

    if let Some(parent) = cache_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(serialized) = serde_json::to_string_pretty(&cache) {
        let _ = fs::File::create(&cache_path)
            .and_then(|mut file| file.write_all(serialized.as_bytes()));
    }

    Ok(cache)
}

fn source_fingerprints(savfox_home: &Path) -> BTreeMap<String, u64> {
    let mut map = BTreeMap::new();
    map.insert(
        "config.toml".to_string(),
        file_mtime_secs(&savfox_home.join("config.toml")),
    );
    map.insert(
        "models.json".to_string(),
        file_mtime_secs(&savfox_home.join("models.json")),
    );
    map.insert(
        "sessions.json".to_string(),
        file_mtime_secs(&savfox_home.join("sessions").join("sessions.json")),
    );
    map.insert(
        "agents_dir".to_string(),
        directory_fingerprint(&savfox_home.join("agents")),
    );
    map
}

fn file_mtime_secs(path: &Path) -> u64 {
    let Ok(meta) = fs::metadata(path) else {
        return 0;
    };
    let Ok(modified) = meta.modified() else {
        return 0;
    };
    modified
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn directory_fingerprint(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut hasher = DefaultHasher::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .hash(&mut hasher);
        file_mtime_secs(&path).hash(&mut hasher);
    }
    hasher.finish()
}

fn collect_models(savfox_home: &Path) -> Vec<String> {
    let mut values = BTreeSet::new();

    let models_path = savfox_home.join("models.json");
    if let Ok(raw) = fs::read_to_string(models_path) {
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            if let Some(obj) = value.as_object() {
                for (key, model) in obj {
                    if !key.trim().is_empty() {
                        values.insert(key.to_string());
                    }
                    if let Some(id) = model.get("id").and_then(Value::as_str) {
                        if !id.trim().is_empty() {
                            values.insert(id.trim().to_string());
                        }
                    }
                }
            }
        }
    }

    let config_path = savfox_home.join("config.toml");
    if let Ok(raw) = fs::read_to_string(config_path) {
        if let Ok(value) = raw.parse::<toml::Value>() {
            if let Some(model) = value.get("model").and_then(toml::Value::as_str) {
                if !model.trim().is_empty() {
                    values.insert(model.trim().to_string());
                }
            }
            if let Some(models) = value.get("models").and_then(toml::Value::as_table) {
                if let Some(primary) = models.get("primary").and_then(toml::Value::as_str) {
                    if !primary.trim().is_empty() {
                        values.insert(primary.trim().to_string());
                    }
                }
                if let Some(fallbacks) = models.get("fallbacks").and_then(toml::Value::as_array) {
                    for fallback in fallbacks {
                        if let Some(id) = fallback.as_str() {
                            if !id.trim().is_empty() {
                                values.insert(id.trim().to_string());
                            }
                        }
                    }
                }
                for key in models.keys() {
                    if key != "primary" && key != "fallbacks" && !key.trim().is_empty() {
                        values.insert(key.to_string());
                    }
                }
            }
        }
    }

    values.into_iter().collect()
}

fn collect_agents(savfox_home: &Path) -> Vec<String> {
    let mut values = BTreeSet::new();
    let dir = savfox_home.join("agents");
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            if !stem.trim().is_empty() {
                values.insert(stem.to_string());
            }
        }
    }
    values.into_iter().collect()
}

fn collect_sessions(savfox_home: &Path) -> Vec<String> {
    let mut values = BTreeSet::new();
    let path = savfox_home.join("sessions").join("sessions.json");
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    if let Ok(value) = serde_json::from_str::<Value>(&raw) {
        if let Some(entries) = value.as_object() {
            for (key, entry) in entries {
                if !key.trim().is_empty() {
                    values.insert(key.to_string());
                }
                if let Some(session_id) = entry.get("session_id").and_then(Value::as_str) {
                    if !session_id.trim().is_empty() {
                        values.insert(session_id.trim().to_string());
                    }
                }
            }
        }
    }
    values.into_iter().collect()
}

const BASH_DYNAMIC_SNIPPET: &str = r#"

# savfox dynamic completion values
__savfox_dynamic_values() {
  savfox completion --dynamic-kind "$1" 2>/dev/null
}

__savfox_complete_with_dynamic() {
  local cur prev
  cur="${COMP_WORDS[COMP_CWORD]}"
  prev="${COMP_WORDS[COMP_CWORD-1]}"

  case "$prev" in
    --model|--model-id|--primary-model)
      COMPREPLY=( $(compgen -W "$(__savfox_dynamic_values models)" -- "$cur") )
      return
      ;;
    --agent|--agent-id)
      COMPREPLY=( $(compgen -W "$(__savfox_dynamic_values agents)" -- "$cur") )
      return
      ;;
    --session|--session-id|--session-key|--session-id)
      COMPREPLY=( $(compgen -W "$(__savfox_dynamic_values sessions)" -- "$cur") )
      return
      ;;
  esac

  if declare -F _savfox >/dev/null; then
    _savfox
  fi
}

complete -o bashdefault -o default -F __savfox_complete_with_dynamic savfox
"#;

const ZSH_DYNAMIC_SNIPPET: &str = r#"

# savfox dynamic completion values
__savfox_dynamic_values() {
  savfox completion --dynamic-kind "$1" 2>/dev/null
}

__savfox_dynamic_dispatch() {
  local prev="${words[CURRENT-1]}"
  local -a suggestions
  case "$prev" in
    --model|--model-id|--primary-model)
      suggestions=(${(f)"$(__savfox_dynamic_values models)"})
      compadd -a suggestions
      return
      ;;
    --agent|--agent-id)
      suggestions=(${(f)"$(__savfox_dynamic_values agents)"})
      compadd -a suggestions
      return
      ;;
    --session|--session-id|--session-key|--session-id)
      suggestions=(${(f)"$(__savfox_dynamic_values sessions)"})
      compadd -a suggestions
      return
      ;;
  esac

  if typeset -f _savfox >/dev/null; then
    _savfox "$@"
  fi
}

compdef __savfox_dynamic_dispatch savfox
"#;

const FISH_DYNAMIC_SNIPPET: &str = r#"

function __savfox_dynamic_values
    savfox completion --dynamic-kind $argv[1] 2>/dev/null
end

complete -c savfox -n '__fish_prev_arg_in --model --model-id --primary-model' -a '(__savfox_dynamic_values models)'
complete -c savfox -n '__fish_prev_arg_in --agent --agent-id' -a '(__savfox_dynamic_values agents)'
complete -c savfox -n '__fish_prev_arg_in --session --session-id --session-key --session-id' -a '(__savfox_dynamic_values sessions)'
"#;

const POWERSHELL_DYNAMIC_SNIPPET: &str = r#"

$__savfoxDynamicCompleter = {
    param($wordToComplete, $commandAst, $cursorPosition)
    $elements = $commandAst.CommandElements
    if ($elements.Count -lt 2) { return }

    $prev = ""
    if ($elements.Count -ge 2) {
        $prev = $elements[$elements.Count - 2].ToString()
    }

    $kind = switch ($prev) {
        "--model" { "models" }
        "--model-id" { "models" }
        "--primary-model" { "models" }
        "--agent" { "agents" }
        "--agent-id" { "agents" }
        "--session" { "sessions" }
        "--session-id" { "sessions" }
        "--session-key" { "sessions" }
        "--session-id" { "sessions" }
        default { $null }
    }

    if (-not $kind) { return }

    & savfox completion --dynamic-kind $kind 2>$null | ForEach-Object {
        if ($_ -like "$wordToComplete*") {
            [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
        }
    }
}

Register-ArgumentCompleter -Native -CommandName savfox -ScriptBlock $__savfoxDynamicCompleter
"#;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::TempDir;

    use super::{Shell, load_or_refresh_completion_cache, render_completion_script};

    #[test]
    fn renders_completion_for_supported_shells() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell] {
            let script = render_completion_script(shell).expect("script render");
            assert!(script.contains("savfox"));
            assert!(script.contains("daemon"));
            assert!(script.contains("sessions"));
            assert!(script.contains("agents"));
        }
    }

    #[test]
    fn completion_cache_refreshes_on_config_change() {
        let tmp = TempDir::new().expect("temp dir");
        let home = tmp.path();
        std::fs::create_dir_all(home.join("agents")).expect("agents dir");
        std::fs::create_dir_all(home.join("sessions")).expect("sessions dir");
        std::fs::write(
            home.join("models.json"),
            r#"{
  "openai/gpt-4o": { "id": "openai/gpt-4o" }
}"#,
        )
        .expect("write models");
        std::fs::write(home.join("config.toml"), "model = \"openai/gpt-4o\"\n")
            .expect("write config");

        let cache1 = load_or_refresh_completion_cache(home, false).expect("cache1");
        assert!(cache1.models.iter().any(|m| m == "openai/gpt-4o"));
        let first_mtime = *cache1
            .source_fingerprints
            .get("config.toml")
            .expect("config fingerprint");

        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(home.join("config.toml"), "model = \"openai/gpt-4.1\"\n")
            .expect("rewrite config");

        let cache2 = load_or_refresh_completion_cache(home, false).expect("cache2");
        let second_mtime = *cache2
            .source_fingerprints
            .get("config.toml")
            .expect("config fingerprint");
        assert_ne!(first_mtime, second_mtime);
    }

    #[test]
    fn completion_cache_collects_agents_and_sessions() {
        let tmp = TempDir::new().expect("temp dir");
        let home = tmp.path();
        std::fs::create_dir_all(home.join("agents")).expect("agents dir");
        std::fs::create_dir_all(home.join("sessions")).expect("sessions dir");
        std::fs::write(home.join("agents").join("alpha.json"), "{}").expect("write agent");
        std::fs::write(
            home.join("sessions").join("sessions.json"),
            r#"{
  "session-1": { "session_id": "sess-1" },
  "session-2": { "session_id": "sess-2" }
}"#,
        )
        .expect("write sessions");

        let cache = load_or_refresh_completion_cache(home, false).expect("cache");
        assert!(cache.agents.iter().any(|v| v == "alpha"));
        assert!(cache.sessions.iter().any(|v| v == "session-1"));
        assert!(cache.sessions.iter().any(|v| v == "sess-1"));
    }
}
