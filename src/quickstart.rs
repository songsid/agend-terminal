//! Interactive quickstart — detect backends, configure Telegram, generate fleet.yaml.

use crate::backend::Backend;
use std::io::{self, Write};
use std::path::Path;

pub fn run(home: &Path) -> anyhow::Result<()> {
    println!("\n  AgEnD Terminal — Quickstart\n");

    // Step 1: Detect backends
    let backends = detect_backends();
    if backends.is_empty() {
        println!("  No supported backends found. Install one of:");
        println!("    npm install -g @anthropic-ai/claude-code");
        println!("    npm install -g @anthropic-ai/codex");
        println!("    npm install -g @anthropic-ai/gemini-cli");
        println!();
        return Ok(());
    }

    let selected = if backends.len() == 1 {
        println!("  ✓ Detected: {}\n", backends[0].name());
        backends[0].clone()
    } else {
        println!("  Detected {} backends:", backends.len());
        for (i, b) in backends.iter().enumerate() {
            let version = b.get_version().unwrap_or_else(|| "?".into());
            println!("    {}. {} (v{})", i + 1, b.name(), version);
        }
        let choice = prompt(&format!("\n  Select backend [1-{}]: ", backends.len()))?;
        let idx: usize = choice.trim().parse().unwrap_or(1);
        let idx = idx.saturating_sub(1).min(backends.len() - 1);
        println!("  ✓ Selected: {}\n", backends[idx].name());
        backends[idx].clone()
    };

    // Step 2: Check existing .env for token
    let env_path = home.join(".env");
    let existing_token = std::fs::read_to_string(&env_path)
        .ok()
        .and_then(|content| {
            content
                .lines()
                .find(|l| l.starts_with("AGEND_BOT_TOKEN="))
                .map(|l| l.trim_start_matches("AGEND_BOT_TOKEN=").trim().to_string())
        })
        .filter(|t| !t.is_empty());

    // Step 3: Check existing fleet.yaml for group_id
    let fleet_path = home.join("fleet.yaml");
    let existing_group_id = std::fs::read_to_string(&fleet_path)
        .ok()
        .and_then(|content| serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&content).ok())
        .and_then(|config| config["channel"]["group_id"].as_i64());

    let (token, group_id, user_id) = if existing_token.is_some() && existing_group_id.is_some() {
        let tok = existing_token.clone().unwrap_or_default();
        let gid = existing_group_id.unwrap_or(0);
        println!("  ── Telegram ──\n");
        println!("  ✓ Token: {}\n  ✓ Group: {gid}", mask_token(&tok));
        let answer = prompt("\n  Use existing Telegram config? (Y/n): ")?;
        if answer.trim().eq_ignore_ascii_case("n") {
            telegram_setup(home)?
        } else {
            println!();
            (tok, Some(gid), None)
        }
    } else if let Some(tok) = existing_token {
        println!("  ── Telegram ──\n");
        println!("  ✓ Token found: {}", mask_token(&tok));
        let answer = prompt("  Use existing token? (Y/n): ")?;
        if answer.trim().eq_ignore_ascii_case("n") {
            telegram_setup(home)?
        } else {
            println!("\n  Add the bot to your Telegram group and send a message.\n");
            print!("  Waiting for group message (3 min timeout)... ");
            io::stdout().flush().ok();
            match detect_group(&tok) {
                Ok(result) => {
                    println!("✓ {} ({})\n", result.group_title, result.group_id);
                    let user_id = if let Some((uid, name)) = result.user {
                        confirm_user_allowlist(uid, &name)?
                    } else {
                        None
                    };
                    (tok, Some(result.group_id), user_id)
                }
                Err(e) => {
                    println!("timeout: {e}\n");
                    (tok, None, None)
                }
            }
        }
    } else {
        telegram_setup(home)?
    };

    // Step 4: Configure fleet params
    let fleet_params = configure_fleet_params(&selected)?;

    // Save .env + fleet.yaml
    if !token.is_empty() {
        save_env_token(home, &token)?;
    }
    generate_fleet_yaml(
        home,
        &selected,
        group_id,
        if token.is_empty() { None } else { Some(&token) },
        user_id,
        &fleet_params,
    )?;

    print_next_steps(home);
    Ok(())
}

/// Full Telegram setup flow — BotFather → token → group detection.
/// Returns (token, group_id, user_id_for_allowlist).
fn telegram_setup(_home: &Path) -> anyhow::Result<(String, Option<i64>, Option<i64>)> {
    println!("  ── Telegram Setup ──\n");
    println!("  1. Open Telegram, talk to @BotFather");
    println!("  2. Send /newbot and follow instructions");
    println!("  3. Copy the bot token\n");

    let token = prompt("  Bot token (Enter to skip): ")?;
    let token = token.trim().to_string();

    if token.is_empty() {
        println!("\n  Skipping Telegram. Configure later in fleet.yaml.\n");
        return Ok((String::new(), None, None));
    }

    // M1: validate telegram bot token format: <digits>:<alphanumeric+_->
    let valid_format = token.split_once(':').is_some_and(|(num, rest)| {
        num.len() >= 8
            && num.chars().all(|c| c.is_ascii_digit())
            && rest.len() >= 30
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    });
    if !valid_format {
        println!(
            "  ⚠ Token format looks wrong (expected <digits>:<35+ chars>). Continuing anyway.\n"
        );
    }

    print!("  Verifying bot... ");
    io::stdout().flush().ok();
    match verify_bot(&token) {
        Ok(bot_name) => println!("✓ @{bot_name}\n"),
        Err(e) => println!("⚠ {e}\n"),
    }

    println!("  Add the bot to your Telegram group (as admin).");
    println!("  Then send any message in the group.\n");
    print!("  Waiting for group message (3 min timeout)... ");
    io::stdout().flush().ok();

    match detect_group(&token) {
        Ok(result) => {
            println!("✓ {} ({})\n", result.group_title, result.group_id);
            let user_id = if let Some((uid, name)) = result.user {
                confirm_user_allowlist(uid, &name)?
            } else {
                None
            };
            Ok((token, Some(result.group_id), user_id))
        }
        Err(e) => {
            println!("timeout: {e}\n");
            println!("  Set group_id manually in fleet.yaml later.\n");
            Ok((token, None, None))
        }
    }
}

fn mask_token(tok: &str) -> String {
    if tok.len() > 8 {
        format!("{}...{}", &tok[..4], &tok[tok.len() - 4..])
    } else {
        "****".to_string()
    }
}

fn detect_backends() -> Vec<Backend> {
    Backend::all()
        .iter()
        .filter(|b| b.is_installed())
        .cloned()
        .collect()
}

fn prompt(msg: &str) -> anyhow::Result<String> {
    print!("{msg}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input)
}

fn verify_bot(token: &str) -> anyhow::Result<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let url = format!("https://api.telegram.org/bot{token}/getMe");
        let resp: serde_json::Value = reqwest::get(&url).await?.json().await?;
        if resp["ok"].as_bool() == Some(true) {
            let username = resp["result"]["username"].as_str().unwrap_or("unknown");
            Ok(username.to_string())
        } else {
            anyhow::bail!(
                "Invalid token: {}",
                resp["description"].as_str().unwrap_or("unknown error")
            )
        }
    })
}

/// Detected group info including optional sender (user) info.
struct DetectGroupResult {
    group_id: i64,
    group_title: String,
    user: Option<(i64, String)>,
}

fn detect_group(token: &str) -> anyhow::Result<DetectGroupResult> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let url = format!("https://api.telegram.org/bot{token}/getUpdates?timeout=180&allowed_updates=[\"message\"]");
        let resp: serde_json::Value = reqwest::get(&url).await?.json().await?;
        if let Some(updates) = resp["result"].as_array() {
            for update in updates {
                if let Some(msg) = update.get("message") {
                    if let Some(chat) = msg.get("chat") {
                        let chat_type = chat["type"].as_str().unwrap_or("");
                        if chat_type == "supergroup" || chat_type == "group" {
                            let id = chat["id"].as_i64().unwrap_or(0);
                            let title = chat["title"].as_str().unwrap_or("Unknown Group").to_string();
                            let user = msg.get("from").and_then(|from| {
                                let uid = from["id"].as_i64()?;
                                let name = from["first_name"].as_str().unwrap_or("Unknown").to_string();
                                Some((uid, name))
                            });
                            return Ok(DetectGroupResult { group_id: id, group_title: title, user });
                        }
                    }
                }
            }
        }
        anyhow::bail!("No group message received")
    })
}

/// Prompt user to confirm adding detected user to allowlist.
/// Returns Some((user_id, name)) if confirmed, None if skipped.
fn confirm_user_allowlist(user_id: i64, name: &str) -> anyhow::Result<Option<i64>> {
    let answer = prompt(&format!(
        "  Add {name} (ID: {user_id}) to allowlist? (Y/n/skip): "
    ))?;
    let trimmed = answer.trim().to_lowercase();
    if trimmed.is_empty() || trimmed == "y" {
        Ok(Some(user_id))
    } else if trimmed == "skip" {
        Ok(None)
    } else {
        Ok(None) // 'n' — caller should continue polling
    }
}

/// Parameters collected from interactive fleet configuration prompts.
struct FleetParams {
    working_dir: std::path::PathBuf,
    instance_count: usize,
    model: Option<String>,
}

fn configure_fleet_params(backend: &Backend) -> anyhow::Result<FleetParams> {
    println!("  ── Fleet Configuration ──\n");

    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let dir_input = prompt(&format!("  Project directory [{}]: ", cwd.display()))?;
    let working_dir = if dir_input.trim().is_empty() {
        cwd
    } else {
        std::path::PathBuf::from(dir_input.trim())
    };

    let count_input = prompt("  Number of agents [1]: ")?;
    let instance_count = count_input.trim().parse::<usize>().unwrap_or(1).max(1);

    // All backends support --model
    let model_input = prompt(&format!("  Model for {} (Enter to skip): ", backend.name()))?;
    let model = if model_input.trim().is_empty() {
        None
    } else {
        Some(model_input.trim().to_string())
    };

    println!();
    Ok(FleetParams { working_dir, instance_count, model })
}

fn generate_fleet_yaml(
    home: &Path,
    backend: &Backend,
    group_id: Option<i64>,
    _token: Option<&str>,
    user_id: Option<i64>,
    params: &FleetParams,
) -> anyhow::Result<()> {
    let fleet_path = home.join("fleet.yaml");

    if fleet_path.exists() {
        // Check compatibility with existing config
        if let Ok(content) = std::fs::read_to_string(&fleet_path) {
            check_compatibility(&content, backend, group_id);
        }

        let answer = prompt("  fleet.yaml already exists. Overwrite? (y/N): ")?;
        if !answer.trim().eq_ignore_ascii_case("y") {
            println!("  Keeping existing fleet.yaml.\n");
            return Ok(());
        }

        // Backup before overwriting
        let backup = home.join("fleet.yaml.bak");
        std::fs::copy(&fleet_path, &backup)?;
        println!("  ✓ Backed up to {}\n", backup.display());
    }

    let backend_name = backend.name();

    let channel_section = if let Some(gid) = group_id {
        let allowlist = if let Some(uid) = user_id {
            format!("  user_allowlist: [{uid}]")
        } else {
            "  user_allowlist: []  # add your Telegram user_id (message @userinfobot to get it)".to_string()
        };
        format!(
            r#"
channel:
  type: telegram
  bot_token_env: AGEND_BOT_TOKEN
  group_id: {gid}
  mode: topic
{allowlist}
"#
        )
    } else {
        "\n# channel:\n#   type: telegram\n#   bot_token_env: AGEND_BOT_TOKEN\n#   group_id: YOUR_GROUP_ID\n#   user_allowlist: [YOUR_USER_ID]\n".to_string()
    };

    let model_line = params.model.as_ref().map(|m| format!("\n    model: {m}")).unwrap_or_default();
    let working_dir_str = params.working_dir.display().to_string();

    let instances_section = if params.instance_count == 1 {
        format!(
            r#"instances:
  general:
    role: "General assistant"
    working_directory: {working_dir_str}{model_line}
"#
        )
    } else {
        let mut lines = String::from("instances:\n");
        for i in 1..=params.instance_count {
            lines.push_str(&format!(
                "  dev-{i}:\n    role: \"Developer agent\"\n    working_directory: {working_dir_str}{model_line}\n"
            ));
        }
        lines
    };

    let yaml = format!(
        r#"defaults:
  backend: {backend_name}
{channel_section}
{instances_section}"#
    );

    std::fs::write(&fleet_path, &yaml)?;
    println!("  ✓ Generated {}\n", fleet_path.display());

    Ok(())
}

/// Save AGEND_BOT_TOKEN to .env, preserving other variables.
fn save_env_token(home: &Path, token: &str) -> anyhow::Result<()> {
    let env_path = home.join(".env");
    let existing = std::fs::read_to_string(&env_path).unwrap_or_default();
    let existing_token = existing
        .lines()
        .find(|l| l.starts_with("AGEND_BOT_TOKEN="))
        .map(|l| l.trim_start_matches("AGEND_BOT_TOKEN=").trim());

    if let Some(old) = existing_token {
        if old == token {
            println!("  ✓ Token unchanged in .env\n");
            return Ok(());
        }
        println!("  .env already has AGEND_BOT_TOKEN={}", mask_token(old));
        let answer = prompt("  Update token? (Y/n): ")?;
        if answer.trim().eq_ignore_ascii_case("n") {
            println!("  Keeping existing token.\n");
            return Ok(());
        }
    }

    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| !l.starts_with("AGEND_BOT_TOKEN="))
        .map(|l| l.to_string())
        .collect();
    lines.push(format!("AGEND_BOT_TOKEN={token}"));
    std::fs::write(&env_path, lines.join("\n") + "\n")?;
    println!("  ✓ Token saved to {}\n", env_path.display());
    Ok(())
}

fn check_compatibility(yaml_content: &str, new_backend: &Backend, new_group_id: Option<i64>) {
    if let Ok(config) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(yaml_content) {
        // Check backend
        let existing_backend = config["defaults"]["backend"].as_str().unwrap_or("");
        if !existing_backend.is_empty() && existing_backend != new_backend.name() {
            println!(
                "  ⚠ Existing backend: {existing_backend}, new: {}",
                new_backend.name()
            );
        }

        // Check group_id
        if let Some(new_gid) = new_group_id {
            let existing_gid = config["channel"]["group_id"].as_i64().unwrap_or(0);
            if existing_gid != 0 && existing_gid != new_gid {
                println!("  ⚠ Existing group_id: {existing_gid}, new: {new_gid}");
            }
        }

        // Check instance count
        if let Some(instances) = config["instances"].as_mapping() {
            if instances.len() > 1 {
                println!(
                    "  ⚠ Existing config has {} instances (new config will have 1)",
                    instances.len()
                );
            }
        }
    }
}

fn print_next_steps(home: &Path) {
    println!("  ── Next Steps ──\n");
    println!("  1. Edit fleet.yaml to add more instances:");
    println!("     {}\n", home.join("fleet.yaml").display());
    println!("  2. Start the fleet:");
    println!("     agend-terminal start\n");
    println!("  3. Check agent status:");
    println!("     agend-terminal status\n");
    println!("  4. Attach to an agent:");
    println!("     agend-terminal attach general\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_token_long() {
        let masked = mask_token("1234567890abcdef");
        assert_eq!(masked, "1234...cdef");
    }

    #[test]
    fn mask_token_short() {
        let masked = mask_token("abcd");
        assert_eq!(masked, "****");
    }

    #[test]
    fn mask_token_exactly_8() {
        let masked = mask_token("12345678");
        assert_eq!(masked, "****");
    }

    #[test]
    fn mask_token_9_chars() {
        let masked = mask_token("123456789");
        assert_eq!(masked, "1234...6789");
    }

    #[test]
    fn detect_backends_does_not_panic() {
        let backends = detect_backends();
        // Should return 0 or more backends without panicking
        assert!(backends.len() <= 5);
    }

    /// Snapshot test: emitted YAML with Telegram channel includes
    /// `user_allowlist` (Sprint 21 fail-closed requirement).
    #[test]
    fn emitted_yaml_with_channel_includes_user_allowlist() {
        let home =
            std::env::temp_dir().join(format!("agend-quickstart-test-{}", std::process::id()));
        std::fs::create_dir_all(&home).ok();
        let backend = Backend::all()[0].clone();
        let params = FleetParams {
            working_dir: home.join("workspace"),
            instance_count: 1,
            model: None,
        };
        generate_fleet_yaml(&home, &backend, Some(-1001234567890), None, None, &params)
            .expect("test");
        let yaml = std::fs::read_to_string(home.join("fleet.yaml")).expect("test");
        assert!(
            yaml.contains("user_allowlist"),
            "emitted fleet.yaml must include user_allowlist for Sprint 21 fail-closed; got:\n{yaml}"
        );
        // Verify it parses as valid FleetConfig.
        let config: crate::fleet::FleetConfig =
            serde_yaml_ng::from_str(&yaml).expect("emitted YAML must parse as FleetConfig");
        assert!(config.channel.is_some(), "channel section must be present");
        assert!(
            config.instances.contains_key("general"),
            "general instance must be present"
        );
        std::fs::remove_dir_all(&home).ok();
    }

    /// Snapshot test: commented-out channel section also mentions
    /// user_allowlist so operators know to add it.
    #[test]
    fn emitted_yaml_without_channel_mentions_user_allowlist() {
        let home = std::env::temp_dir().join(format!(
            "agend-quickstart-test-nogroup-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&home).ok();
        let backend = Backend::all()[0].clone();
        let params = FleetParams {
            working_dir: home.join("workspace"),
            instance_count: 1,
            model: None,
        };
        generate_fleet_yaml(&home, &backend, None, None, None, &params).expect("test");
        let yaml = std::fs::read_to_string(home.join("fleet.yaml")).expect("test");
        assert!(
            yaml.contains("user_allowlist"),
            "commented-out channel section must mention user_allowlist; got:\n{yaml}"
        );
        std::fs::remove_dir_all(&home).ok();
    }

    /// Test: user_id is emitted in allowlist when provided.
    #[test]
    fn emitted_yaml_with_user_id_in_allowlist() {
        let home = std::env::temp_dir().join(format!(
            "agend-quickstart-test-uid-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&home).ok();
        let backend = Backend::all()[0].clone();
        let params = FleetParams {
            working_dir: home.join("workspace"),
            instance_count: 1,
            model: None,
        };
        generate_fleet_yaml(&home, &backend, Some(-1001234567890), None, Some(123456789), &params)
            .expect("test");
        let yaml = std::fs::read_to_string(home.join("fleet.yaml")).expect("test");
        assert!(
            yaml.contains("user_allowlist: [123456789]"),
            "user_allowlist must contain the detected user_id; got:\n{yaml}"
        );
        std::fs::remove_dir_all(&home).ok();
    }

    /// Test: multi-instance config generates dev-1, dev-2, etc.
    #[test]
    fn emitted_yaml_multi_instance() {
        let home = std::env::temp_dir().join(format!(
            "agend-quickstart-test-multi-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&home).ok();
        let backend = Backend::all()[0].clone();
        let params = FleetParams {
            working_dir: home.join("workspace"),
            instance_count: 3,
            model: Some("opus".to_string()),
        };
        generate_fleet_yaml(&home, &backend, Some(-100123), None, None, &params).expect("test");
        let yaml = std::fs::read_to_string(home.join("fleet.yaml")).expect("test");
        assert!(yaml.contains("dev-1:"), "must have dev-1; got:\n{yaml}");
        assert!(yaml.contains("dev-2:"), "must have dev-2; got:\n{yaml}");
        assert!(yaml.contains("dev-3:"), "must have dev-3; got:\n{yaml}");
        assert!(yaml.contains("model: opus"), "must have model; got:\n{yaml}");
        std::fs::remove_dir_all(&home).ok();
    }
}
