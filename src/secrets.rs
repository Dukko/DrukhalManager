//! API keys in the system keyring: the Secret Service (GNOME Keyring,
//! KWallet, oo7) natively, or the Secret portal's encrypted file in the
//! Flatpak.
//!
//! Every function blocks, possibly on an unlock prompt; call them off the
//! main thread.

use futures_lite::future::block_on;

use crate::merge::ai::Provider;

/// The Steam Web API key's kind, for reading Workshop required items.
pub const STEAM: &str = "steam";

/// Which provider a key belongs to, as stored in the keyring.
pub fn kind(provider: &Provider) -> &'static str {
    match provider {
        Provider::Anthropic { .. } => "anthropic",
        Provider::OpenAiCompatible { .. } => "openai-compatible",
    }
}

fn label(kind: &str) -> String {
    let name = match kind {
        "anthropic" => "Claude",
        STEAM => "Steam Web",
        _ => "OpenAI-compatible",
    };
    format!("DrukhalManager {name} API key")
}

fn attributes(kind: &str) -> [(&'static str, &str); 2] {
    [("application", crate::APP_ID), ("provider", kind)]
}

fn keyring() -> Result<oo7::Keyring, String> {
    block_on(async {
        let keyring = oo7::Keyring::new().await?;
        keyring.unlock().await?;
        Ok(keyring)
    })
    .map_err(|e: oo7::Error| format!("No system keyring is available ({e})"))
}

/// The key stored for `kind`, if any.
pub fn get(kind: &str) -> Result<Option<String>, String> {
    let keyring = keyring()?;
    block_on(async {
        let Some(item) = keyring.search_items(&attributes(kind)).await?.into_iter().next() else { return Ok(None) };
        let secret = item.secret().await?;
        Ok(Some(String::from_utf8_lossy(secret.as_bytes()).into_owned()))
    })
    .map_err(|e: oo7::Error| format!("Could not read the API key from the keyring: {e}"))
}

/// Stores `key` for `kind`, replacing any earlier one; an empty key removes
/// it.
pub fn set(kind: &str, key: &str) -> Result<(), String> {
    let keyring = keyring()?;
    block_on(async {
        if key.is_empty() {
            keyring.delete(&attributes(kind)).await
        } else {
            keyring.create_item(&label(kind), &attributes(kind), key, true).await
        }
    })
    .map_err(|e| format!("Could not save the API key in the keyring: {e}"))
}

#[cfg(test)]
mod tests {
    /// Uses the real keyring, under a throwaway provider name. Run with
    /// `cargo test -- --ignored keyring`.
    #[test]
    #[ignore]
    fn keyring_round_trip() {
        let kind = "drukhal-selftest";
        super::set(kind, "first").unwrap();
        super::set(kind, "second").unwrap();
        assert_eq!(super::get(kind).unwrap().as_deref(), Some("second"), "replaced, not duplicated");
        super::set(kind, "").unwrap();
        assert_eq!(super::get(kind).unwrap(), None);
    }
}
