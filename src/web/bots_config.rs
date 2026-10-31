// ---------- bots.toml writer/validator cluster ----------
//
// Everything the add-bot handler needs to decide between writing
// BOTS_CONFIG directly and falling back to the copy-paste snippet, plus the
// staged temp/backup/rename writer itself. Moved verbatim from
// admin_console.rs's bots cluster (P2-2a); the 0600-at-creation fix (P1-2)
// is the one behavior change, applied to the moved code. Self-contained by
// design: no `web` module state is needed here.

/// Where the bot config file lives, from `BOTS_CONFIG` (unset/empty = none).
pub(super) fn bots_config_path() -> Option<std::path::PathBuf> {
    let p = std::env::var("BOTS_CONFIG").ok()?;
    let p = p.trim();
    if p.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(p))
}

fn parent_writable(p: &std::path::Path) -> bool {
    let parent = p.parent().unwrap_or_else(|| std::path::Path::new("."));
    if !parent.is_dir() {
        return false;
    }
    let probe = parent.join(format!(".bots_probe_{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Everything the add handler needs to know about the bot config file
/// (BOTS_CONFIG) before it can choose between writing and the snippet
/// fallback. `force_fallback` mirrors the original handler's conditions:
/// no path configured, an unreadable/non-file target, or unparseable
/// existing content.
pub(super) struct BotConfigState {
    pub(super) path: Option<std::path::PathBuf>,
    pub(super) content: String,
    pub(super) writable: bool,
    pub(super) force_fallback: bool,
}

#[tracing::instrument]
pub(super) fn bot_config_state() -> BotConfigState {
    let path = bots_config_path();
    let writable = path
        .as_ref()
        .map(|p| {
            if p.exists() {
                p.is_file() && std::fs::OpenOptions::new().append(true).open(p).is_ok()
            } else {
                true
            }
        })
        .unwrap_or(false)
        && path.as_ref().map(|p| parent_writable(p)).unwrap_or(false);
    let content = path
        .as_ref()
        .filter(|p| p.exists())
        .map(|p| std::fs::read_to_string(p).unwrap_or_default())
        .unwrap_or_default();
    let mut force_fallback = path.is_none();
    if let Some(p) = &path {
        if p.exists() && std::fs::read_to_string(p).is_err() {
            force_fallback = true;
        }
        if p.exists() && !p.is_file() {
            force_fallback = true;
        }
    }
    if !force_fallback && !content.trim().is_empty() && parse_toml_document(&content).is_err() {
        force_fallback = true;
    }
    BotConfigState {
        path,
        content,
        writable,
        force_fallback,
    }
}

/// Parse a whole bots.toml document (root table). `toml::Value::from_str`
/// only accepts bare values in toml 1.x, so the document parser is the one
/// thing all round-trip/duplicate checks share (see `duplicate_key_in`).
pub(super) fn parse_toml_document(content: &str) -> Result<toml::Table, toml::de::Error> {
    toml::from_str(content)
}

/// Whether `key` already appears in the `[[bots]]` array of a parsed config
/// file body. Empty/unparseable content never duplicates.
pub(super) fn duplicate_key_in(content: &str, key: &str) -> bool {
    if content.trim().is_empty() {
        return false;
    }
    parse_toml_document(content)
        .ok()
        .and_then(|v| {
            v.get("bots").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .any(|b| b.get("key").and_then(|v| v.as_str()) == Some(key))
            })
        })
        .unwrap_or(false)
}

/// TOML basic-string escape for a value written inside `"..."`: backslash,
/// quote and control characters are escaped so any validator-permitted
/// name still yields a parseable document (P2). Without this, a free-form
/// name like `He said "hi"` lands as raw TOML; the direct write path fails
/// safe (parse pre-check), but the snippet fallback hands back an
/// unparseable block — pasted, it makes bots.toml unparseable and
/// resolve_bots silently drops every TOML-configured bot at next boot.
/// key/token_env/default_channel are slug/uppercase/`@`-numeric-validated
/// upstream and cannot carry these bytes; the free-form display name is the
/// one injection surface, so only it is escaped here.
pub(super) fn toml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

pub(super) fn bot_toml_block(
    key: &str,
    name: &str,
    token_env: &str,
    owner: i64,
    dc: Option<&str>,
) -> String {
    let mut toml_block = String::new();
    toml_block.push_str("[[bots]]\n");
    toml_block.push_str(&format!("key = \"{}\"\n", key));
    toml_block.push_str(&format!("name = \"{}\"\n", toml_escape(name)));
    toml_block.push_str(&format!("token_env = \"{}\"\n", token_env));
    toml_block.push_str(&format!("owner_id = {}\n", owner));
    if let Some(v) = dc {
        toml_block.push_str(&format!("default_channel = \"{}\"\n", v));
    }
    toml_block
}

pub(super) fn bot_config_new_content(content_now: &str, toml_block: &str) -> String {
    let mut new_content = content_now.to_string();
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    if !new_content.is_empty() && !new_content.ends_with("\n\n") {
        new_content.push('\n');
    }
    new_content.push_str(toml_block);
    if !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content
}

/// Attempt the real config write: backup → staged temp file (0600) → atomic
/// rename → best-effort chmod. The file is left untouched on failure.
pub(super) enum ConfigWriteResult {
    Written,
    /// The pre-write backup failed: abort outright (the caller reports the
    /// refusal instead of silently falling back).
    BackupFailed,
    /// Temporary-file or rename trouble: the caller re-tries via the
    /// snippet fallback.
    RetryAsFallback,
}

pub(super) fn append_bot_block(p: &std::path::Path, new_content: &str) -> ConfigWriteResult {
    if p.exists() {
        let ts = chrono::Utc::now().timestamp();
        let backup = std::path::PathBuf::from(format!("{}.bak.{}", p.display(), ts));
        if std::fs::copy(p, &backup).is_err() {
            return ConfigWriteResult::BackupFailed;
        }
    }
    let parent = p.parent().unwrap_or_else(|| std::path::Path::new("."));
    let temp = parent.join(format!(".bots_tmp_{}", std::process::id()));
    let write_res = (|| -> Result<(), std::io::Error> {
        use std::io::Write;
        // P1-2: the 0600 mode rides the open(2) O_CREAT mode argument, so
        // token-bearing staged content is never readable at the umask
        // default — the same posture as the house .env writer
        // (config.rs write_env_atomically).
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        file.write_all(new_content.as_bytes())?;
        file.sync_all()?;
        Ok(())
    })();
    if write_res.is_err() {
        let _ = std::fs::remove_file(&temp);
        return ConfigWriteResult::RetryAsFallback;
    }
    if std::fs::rename(&temp, p).is_err() {
        let _ = std::fs::remove_file(&temp);
        return ConfigWriteResult::RetryAsFallback;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
    }
    ConfigWriteResult::Written
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// P1-2 regression: a written bot config lands with mode 0600 (the
    /// staged temp carries the mode from creation through the rename).
    #[test]
    fn written_bot_config_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("bots.toml");
        let block = "[[bots]]\nkey = \"main\"\n";
        let content = bot_config_new_content("", block);
        assert!(matches!(
            append_bot_block(&target, &content),
            ConfigWriteResult::Written
        ));
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "bot config must be owner-only");
    }

    /// P2: the display name is free-form (the add validator permits `"` and
    /// `\`), so bot_toml_block must TOML-escape it — every
    /// validator-permitted name yields a parseable block whose parsed name
    /// round-trips, and the snippet fallback can never hand back a block
    /// that would make bots.toml unparseable.
    #[test]
    fn bot_toml_block_escapes_name_and_round_trips() {
        let name = "He said \"hi\"\\path";
        let block = bot_toml_block("main", name, "BOT_MAIN", 42, None);
        let parsed_name = parse_toml_document(&block)
            .expect("escaped block must parse")
            .get("bots")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|b| b.get("name"))
            .and_then(|v| v.as_str())
            .expect("parsed bot carries a name")
            .to_string();
        assert_eq!(parsed_name, name, "name round-trips through TOML");
        // Control characters ride \uXXXX escapes too, never raw bytes.
        let ctrl = "a\u{1}b";
        let parsed_ctrl = parse_toml_document(&bot_toml_block("main", ctrl, "BOT_MAIN", 42, None))
            .expect("control-char name must parse")
            .get("bots")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|b| b.get("name"))
            .and_then(|v| v.as_str())
            .expect("parsed bot carries a name")
            .to_string();
        assert_eq!(parsed_ctrl, ctrl);
        // A plain name is written through unchanged.
        let plain = bot_toml_block("main", "Shop Bot", "BOT_MAIN", 42, None);
        assert!(plain.contains("name = \"Shop Bot\""), "{plain}");
        assert!(parse_toml_document(&plain).is_ok());
    }
}
