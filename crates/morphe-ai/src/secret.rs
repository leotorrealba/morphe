//! Secret storage for the AI provider credential.
//!
//! The agent loop's [`Client`](crate::Client) needs an API key. Rather than
//! pass it as a bare string through config (which leaks it into process args
//! and env), [`SecretStore`] abstracts get/set/remove so the credential can
//! live in the OS credential store (production) or an in-memory double
//! (tests).
//!
//! The stored string holds a full [`ProviderConfig`] — provider + api_key +
//! optional `base_url`/`model` overrides — serialized as JSON. A legacy bare
//! Anthropic key (the original format) still parses (Anthropic + defaults), so
//! stored keys never needed migration.
//!
//! # Backends
//!
//! - [`InMemorySecretStore`] — a `Mutex<Option<String>>` test double. No
//!   persistence, no keychain access; used by all tests and any headless run
//!   that reads the key from the environment separately.
//! - [`KeychainSecretStore`] — the OS credential store, backed by the
//!   `keyring` crate: the Keychain on macOS, Credential Manager on Windows,
//!   and the Secret Service (GNOME Keyring, KWallet, ...) on Linux. Absent on
//!   other targets. On Linux the Secret Service daemon may be missing at
//!   runtime; [`KeychainSecretStore::probe`] reports that so callers can fall
//!   back to the in-memory store.
//!
//! # Wiring the client
//!
//! [`client_from_secret`] reads the stored provider config from a store and
//! builds a [`Client`](crate::Client), failing if none is set.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::client::{Client, ClientConfig, Provider};

/// The fixed credential-store service name for the morphe AI provider config.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
const KEYCHAIN_SERVICE: &str = "morphe";
/// The fixed credential-store account name for the AI provider credential.
/// Historical name from when the entry held only the Anthropic key — kept
/// unchanged so legacy stored keys resolve to the same entry (no migration).
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
const KEYCHAIN_ACCOUNT: &str = "anthropic-api-key";

/// Error from a [`SecretStore`] backend.
#[derive(Debug, Error)]
pub enum SecretError {
    /// The backend reported no key set (returned by `get_key` as `Ok(None)`
    /// internally; surfaced by [`client_from_secret`] when a key is required).
    #[error("no API key found in secret store")]
    NotFound,
    /// The backend failed to read/write/delete (keychain unavailable,
    /// permission denied, encoding error, etc.).
    #[error("secret store backend error: {0}")]
    Backend(String),
    /// The provider config failed validation (e.g. a base URL that is not
    /// `https://` and not loopback) and was NOT stored.
    #[error("invalid provider config: {0}")]
    InvalidConfig(String),
}

/// Get/set/remove the stored AI provider credential (a serialized
/// [`ProviderConfig`]; a bare Anthropic key in the legacy format).
///
/// `get_key` returns `Ok(None)` when no key is set (a valid state at session
/// start); `Ok(Some(key))` when one is. `remove_key` is idempotent (removing an
/// unset key is `Ok(())`).
pub trait SecretStore: Send + Sync {
    fn get_key(&self) -> Result<Option<String>, SecretError>;
    fn set_key(&self, key: &str) -> Result<(), SecretError>;
    fn remove_key(&self) -> Result<(), SecretError>;
}

// ---------------------------------------------------------------------------
// In-memory test double
// ---------------------------------------------------------------------------

/// An in-memory `Mutex<Option<String>>` secret store — the test double.
///
/// No persistence, no keychain access. Safe to share across threads (the inner
/// `Mutex` synchronizes access).
#[derive(Default)]
pub struct InMemorySecretStore {
    key: Mutex<Option<String>>,
}

impl InMemorySecretStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for InMemorySecretStore {
    fn get_key(&self) -> Result<Option<String>, SecretError> {
        Ok(self.key.lock().expect("in-memory store lock").clone())
    }

    fn set_key(&self, key: &str) -> Result<(), SecretError> {
        *self.key.lock().expect("in-memory store lock") = Some(key.to_string());
        Ok(())
    }

    fn remove_key(&self) -> Result<(), SecretError> {
        *self.key.lock().expect("in-memory store lock") = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// OS credential-store backend (cfg-gated)
// ---------------------------------------------------------------------------

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
mod keychain {
    use super::{SecretError, SecretStore, KEYCHAIN_ACCOUNT, KEYCHAIN_SERVICE};

    use keyring::Entry;

    /// The OS credential-store backend for [`SecretStore`], backed by the
    /// `keyring` crate: the Security framework's generic-password store on
    /// macOS, Credential Manager on Windows, the Secret Service on Linux.
    ///
    /// Constructing this stores no key and touches no credential store; only
    /// [`Self::probe`], [`SecretStore::get_key`], [`SecretStore::set_key`] and
    /// [`SecretStore::remove_key`] access it. The service name (`"morphe"`)
    /// and account (`"anthropic-api-key"`) are fixed.
    pub struct KeychainSecretStore {
        service: &'static str,
        account: &'static str,
    }

    impl KeychainSecretStore {
        #[must_use]
        pub fn new() -> Self {
            Self {
                service: KEYCHAIN_SERVICE,
                account: KEYCHAIN_ACCOUNT,
            }
        }

        fn entry(&self) -> Result<Entry, SecretError> {
            Entry::new(self.service, self.account).map_err(|e| SecretError::Backend(e.to_string()))
        }

        /// Check that the platform credential store is reachable, by looking
        /// up the entry (an absent entry is fine).
        ///
        /// # Errors
        ///
        /// [`SecretError::Backend`] when the store cannot be used at all —
        /// e.g. on Linux when no Secret Service daemon is running on the
        /// session bus, or the bus itself is unavailable.
        ///
        /// Only a platform failure counts as unavailable. Other outcomes (no
        /// entry, a locked store or denied access, an undecodable value) mean
        /// the store exists, so the caller should keep using it.
        pub fn probe(&self) -> Result<(), SecretError> {
            match self.entry()?.get_password() {
                Err(e @ keyring::Error::PlatformFailure(_)) => {
                    Err(SecretError::Backend(e.to_string()))
                }
                _ => Ok(()),
            }
        }
    }

    impl Default for KeychainSecretStore {
        fn default() -> Self {
            Self::new()
        }
    }

    impl SecretStore for KeychainSecretStore {
        fn get_key(&self) -> Result<Option<String>, SecretError> {
            match self.entry()?.get_password() {
                Ok(pw) => Ok(Some(pw)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(SecretError::Backend(e.to_string())),
            }
        }

        fn set_key(&self, key: &str) -> Result<(), SecretError> {
            self.entry()?
                .set_password(key)
                .map_err(|e| SecretError::Backend(e.to_string()))
        }

        fn remove_key(&self) -> Result<(), SecretError> {
            match self.entry()?.delete_credential() {
                Ok(()) => Ok(()),
                // Removing an unset key is a no-op success (idempotent).
                Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(SecretError::Backend(e.to_string())),
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub use keychain::KeychainSecretStore;

// ---------------------------------------------------------------------------
// Client wiring
// ---------------------------------------------------------------------------

/// The stored provider configuration: which provider, the API
/// key, and optional `base_url`/`model` overrides (`None` → the provider's
/// defaults). Serialized to JSON and stored as the secret-store string — ONE
/// keychain entry holds the full config. Backward-compat: a legacy bare
/// key (Anthropic) is read via [`ProviderConfig::from_stored_string`] as
/// Anthropic + defaults.
///
/// `Debug` is implemented by hand so the `api_key` is never surfaced (it
/// would leak into logs, panic messages, and tracing spans) — the same protection
/// `ClientConfig` carries.
#[derive(Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub provider: Provider,
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never surface the api_key in debug output. Show a redaction sentinel
        // so logs and panic messages cannot leak the credential.
        let redacted = if self.api_key.is_empty() {
            "<empty>"
        } else {
            "<redacted>"
        };
        f.debug_struct("ProviderConfig")
            .field("provider", &self.provider)
            .field("api_key", &redacted)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish()
    }
}

impl ProviderConfig {
    /// Build the runtime [`ClientConfig`]: provider + key + base_url/model
    /// (overridden or the provider default) + default max_tokens/timeout.
    /// The 60s timeout is a per-read IDLE timeout (see
    /// [`ClientConfig::timeout`]), not a whole-request deadline.
    #[must_use]
    pub fn to_client_config(&self) -> ClientConfig {
        self.to_client_config_with_tuning(4096, Some(std::time::Duration::from_secs(60)))
    }

    /// [`to_client_config`] with caller-supplied max_tokens/timeout (the
    /// tuning a caller already chose survives; only provider/key/base_url/
    /// model come from the stored config).
    #[must_use]
    pub fn to_client_config_with_tuning(
        &self,
        max_tokens: u32,
        timeout: Option<std::time::Duration>,
    ) -> ClientConfig {
        ClientConfig {
            provider: self.provider,
            api_key: self.api_key.clone(),
            base_url: self
                .base_url
                .clone()
                .unwrap_or_else(|| self.provider.default_base_url().to_string()),
            model: self
                .model
                .clone()
                .unwrap_or_else(|| self.provider.default_model().to_string()),
            max_tokens,
            timeout,
        }
    }

    /// Validate the config before it is stored: a `base_url` override must
    /// pass [`crate::http::validate_base_url`] (`https://`, or `http://` only
    /// for loopback hosts; no userinfo / query / fragment). The API key is sent
    /// to this URL on every request, so this is the set-time check; the
    /// transports re-check before every send (fail closed).
    pub fn validate(&self) -> Result<(), SecretError> {
        if let Some(url) = self.base_url.as_deref() {
            crate::http::validate_base_url(url).map_err(SecretError::InvalidConfig)?;
        }
        Ok(())
    }

    /// Serialize to the stored JSON string. Runs [`ProviderConfig::validate`]
    /// first, so an invalid config can never be written to the store.
    pub fn to_stored_string(&self) -> Result<String, SecretError> {
        self.validate()?;
        serde_json::to_string(self)
            .map_err(|e| SecretError::Backend(format!("serialize ProviderConfig: {e}")))
    }

    /// Parse a stored secret string into a [`ProviderConfig`]. Backward-compat:
    /// a pre-Slice-2 bare key (not JSON) → Anthropic + defaults, so an existing
    /// Anthropic key keeps working. API keys (`sk-…`) are never valid JSON
    /// objects, so the bare-key fallback is safe.
    pub fn from_stored_string(stored: &str) -> Result<Self, SecretError> {
        match serde_json::from_str::<ProviderConfig>(stored) {
            Ok(cfg) => Ok(cfg),
            Err(_) => Ok(ProviderConfig {
                provider: Provider::Anthropic,
                api_key: stored.to_string(),
                base_url: None,
                model: None,
            }),
        }
    }
}

/// Build a [`Client`] by reading the provider config from a [`SecretStore`].
///
/// Reads the stored string; if none is set, returns [`SecretError::NotFound`].
/// The stored [`ProviderConfig`] (JSON, or a bare key → Anthropic + defaults) is
/// the source of truth for provider / api_key / base_url / model — the passed
/// `config` supplies only the remaining defaults (max_tokens/timeout). This
/// keeps the signature stable (no caller breakage) while making the client
/// provider-aware.
pub fn client_from_secret(
    store: &dyn SecretStore,
    config: ClientConfig,
) -> Result<Client, SecretError> {
    let stored = store.get_key()?.ok_or(SecretError::NotFound)?;
    let pc = ProviderConfig::from_stored_string(&stored)?;
    // The stored config is the source of truth for provider/api_key/base_url/
    // model — to_client_config resolves per-provider defaults + overrides in
    // ONE place (no duplicated resolution logic to drift). The caller's config
    // supplies only max_tokens/timeout, carried over explicitly.
    let resolved = pc.to_client_config_with_tuning(config.max_tokens, config.timeout);
    Ok(Client::new(resolved))
}

#[cfg(test)]
mod tests {
    //! SecretStore tests.

    use super::*;

    #[test]
    fn secret_store_roundtrips_key() {
        let store = InMemorySecretStore::new();

        // No key initially.
        assert!(store.get_key().expect("get").is_none());

        // Set → get returns it.
        store.set_key("sk-ant-test-12345").expect("set");
        let got = store.get_key().expect("get after set");
        assert_eq!(got.as_deref(), Some("sk-ant-test-12345"));

        // Remove → get returns None.
        store.remove_key().expect("remove");
        assert!(store.get_key().expect("get after remove").is_none());
    }

    #[test]
    fn remove_key_is_idempotent_when_unset() {
        let store = InMemorySecretStore::new();
        // Removing a key that was never set is Ok (no-op).
        store.remove_key().expect("remove on empty store");
        assert!(store.get_key().expect("get").is_none());
    }

    #[test]
    fn set_key_overwrites_previous() {
        let store = InMemorySecretStore::new();
        store.set_key("first").expect("set first");
        store.set_key("second").expect("set second");
        assert_eq!(store.get_key().expect("get").as_deref(), Some("second"));
    }

    #[test]
    fn client_from_secret_builds_client_when_key_present() {
        let store = InMemorySecretStore::new();
        store.set_key("sk-ant-from-store").expect("set");
        let config = ClientConfig::default();
        let client = client_from_secret(&store, config).expect("client builds");
        assert_eq!(client.config().api_key, "sk-ant-from-store");
    }

    #[test]
    fn client_from_secret_errors_when_no_key() {
        let store = InMemorySecretStore::new();
        let err = client_from_secret(&store, ClientConfig::default())
            .err()
            .expect("no key → error");
        assert!(
            matches!(err, SecretError::NotFound),
            "expected NotFound, got {err:?}"
        );
    }

    /// The macOS Keychain backend must compile and implement SecretStore. This
    /// constructs the backend (no keychain access) and confirms the trait is
    /// satisfied. It runs only on macOS; on other targets the backend is
    /// cfg-gated out entirely.
    /// The api_key must NEVER appear in Debug output.
    /// Deleting the hand-impl Debug (falling back to derive) fails this test
    /// (regression guard against a derived Debug leaking the key).
    #[test]
    fn provider_config_debug_redacts_api_key() {
        let cfg = ProviderConfig {
            provider: Provider::OpenAiCompatible,
            api_key: "canary-not-a-real-credential-9f1e".to_string(),
            base_url: Some("https://openrouter.example".to_string()),
            model: Some("gpt-4o-mini".to_string()),
        };
        let dbg = format!("{cfg:?}");
        assert!(
            !dbg.contains("canary-not-a-real-credential-9f1e"),
            "api_key LEAKED via Debug: {dbg}"
        );
        assert!(
            dbg.contains("<redacted>"),
            "expected redaction sentinel: {dbg}"
        );

        // Empty key shows the empty sentinel (mirrors ClientConfig).
        let empty = ProviderConfig {
            provider: Provider::Anthropic,
            api_key: String::new(),
            base_url: None,
            model: None,
        };
        assert!(format!("{empty:?}").contains("<empty>"));
    }

    // -----------------------------------------------------------------------
    // ProviderConfig
    // -----------------------------------------------------------------------

    /// Round-trip through the store — set_key(to_stored_string) →
    /// get_key → from_stored_string preserves every field, None and Some both.
    #[test]
    fn provider_config_roundtrips_through_store() {
        let store = InMemorySecretStore::new();

        // Full config with overrides.
        let full = ProviderConfig {
            provider: Provider::OpenAiCompatible,
            api_key: "sk-oai-test".to_string(),
            base_url: Some("https://openrouter.example/api".to_string()),
            model: Some("gpt-4o-mini".to_string()),
        };
        store
            .set_key(&full.to_stored_string().expect("serialize"))
            .expect("set");
        let back =
            ProviderConfig::from_stored_string(&store.get_key().expect("get").expect("some"))
                .expect("parse");
        assert_eq!(back.provider, Provider::OpenAiCompatible);
        assert_eq!(back.api_key, "sk-oai-test");
        assert_eq!(
            back.base_url.as_deref(),
            Some("https://openrouter.example/api")
        );
        assert_eq!(back.model.as_deref(), Some("gpt-4o-mini"));

        // Defaults-only config (None overrides serialize compactly via
        // #[serde(default)] and parse back as None).
        let defaults = ProviderConfig {
            provider: Provider::Anthropic,
            api_key: "sk-ant-test".to_string(),
            base_url: None,
            model: None,
        };
        let stored = defaults.to_stored_string().expect("serialize");
        assert!(
            !stored.contains("base_url") && !stored.contains("model"),
            "None overrides should be omitted: {stored}"
        );
        let back = ProviderConfig::from_stored_string(&stored).expect("parse");
        assert_eq!(back.provider, Provider::Anthropic);
        assert_eq!(back.api_key, "sk-ant-test");
        assert_eq!(back.base_url, None);
        assert_eq!(back.model, None);
    }

    /// Backward compat — a legacy bare Anthropic key keeps working.
    #[test]
    fn provider_config_bare_key_backward_compat() {
        // A bare key is not JSON → Anthropic + defaults.
        let pc = ProviderConfig::from_stored_string("sk-ant-legacy-key").expect("parse");
        assert_eq!(pc.provider, Provider::Anthropic);
        assert_eq!(pc.api_key, "sk-ant-legacy-key");
        assert_eq!(pc.base_url, None);
        assert_eq!(pc.model, None);

        // Through the store + client_from_secret: a legacy stored key builds an
        // Anthropic client with the legacy key and Anthropic default
        // base_url/model — no migration needed. The caller's config is
        // OPENAI-flavored here so the STORE wins provably (with
        // an Anthropic caller config the store-vs-caller precedence for the
        // bare-key path is indistinguishable from ClientConfig::default).
        let store = InMemorySecretStore::new();
        store.set_key("sk-ant-legacy-key").expect("set");
        let caller = ClientConfig {
            provider: Provider::OpenAiCompatible,
            base_url: "https://caller.example".to_string(),
            model: "caller-model".to_string(),
            ..ClientConfig::default()
        };
        let client = client_from_secret(&store, caller).expect("client");
        assert_eq!(client.config().provider, Provider::Anthropic);
        assert_eq!(client.config().api_key, "sk-ant-legacy-key");
        assert_eq!(client.config().base_url, "https://api.anthropic.com");
        assert_eq!(client.config().model, "claude-sonnet-5-5");
    }

    /// to_client_config resolves per-provider defaults and honors
    /// overrides.
    #[test]
    fn provider_config_to_client_config_defaults_and_overrides() {
        // Anthropic defaults.
        let anthropic = ProviderConfig {
            provider: Provider::Anthropic,
            api_key: "k1".to_string(),
            base_url: None,
            model: None,
        };
        let cfg = anthropic.to_client_config();
        assert_eq!(cfg.base_url, "https://api.anthropic.com");
        assert_eq!(cfg.model, "claude-sonnet-5-5");

        // OpenAI-compatible defaults.
        let openai = ProviderConfig {
            provider: Provider::OpenAiCompatible,
            api_key: "k2".to_string(),
            base_url: None,
            model: None,
        };
        let cfg = openai.to_client_config();
        assert_eq!(cfg.provider, Provider::OpenAiCompatible);
        assert_eq!(cfg.base_url, "https://api.openai.com");
        assert_eq!(cfg.model, "gpt-4o");

        // Overrides win verbatim (OpenRouter / Azure / local LLMs).
        let overridden = ProviderConfig {
            provider: Provider::OpenAiCompatible,
            api_key: "k3".to_string(),
            base_url: Some("http://localhost:11434/v1".to_string()),
            model: Some("llama3.1".to_string()),
        };
        let cfg = overridden.to_client_config();
        assert_eq!(cfg.base_url, "http://localhost:11434/v1");
        assert_eq!(cfg.model, "llama3.1");
        assert_eq!(cfg.max_tokens, 4096);
    }

    /// client_from_secret — the stored config drives the client; the
    /// caller's max_tokens/timeout survive.
    #[test]
    fn client_from_secret_stored_config_drives_client() {
        let store = InMemorySecretStore::new();
        let stored = ProviderConfig {
            provider: Provider::OpenAiCompatible,
            api_key: "sk-oai-store".to_string(),
            base_url: Some("https://openrouter.example/api".to_string()),
            model: Some("gpt-4o-mini".to_string()),
        };
        store
            .set_key(&stored.to_stored_string().expect("serialize"))
            .expect("set");

        // Caller config carries NON-default max_tokens/timeout — those must
        // survive; provider/key/base_url/model come from the store.
        let caller = ClientConfig {
            max_tokens: 8192,
            timeout: Some(std::time::Duration::from_secs(120)),
            ..ClientConfig::default()
        };

        let client = client_from_secret(&store, caller).expect("client");
        let cfg = client.config();
        assert_eq!(cfg.provider, Provider::OpenAiCompatible);
        assert_eq!(cfg.api_key, "sk-oai-store");
        assert_eq!(cfg.base_url, "https://openrouter.example/api");
        assert_eq!(cfg.model, "gpt-4o-mini");
        assert_eq!(cfg.max_tokens, 8192);
        assert_eq!(cfg.timeout, Some(std::time::Duration::from_secs(120)));
    }

    /// base_url validation at set time: the key must never be stored next to
    /// (and later sent to) a non-https, non-loopback URL.
    #[test]
    fn provider_config_rejects_insecure_base_url_at_store_time() {
        let cfg = |url: &str| ProviderConfig {
            provider: Provider::OpenAiCompatible,
            api_key: "sk-test".to_string(),
            base_url: Some(url.to_string()),
            model: None,
        };
        for bad in [
            "http://evil.example",
            "ftp://example.com",
            "https://user:pw@example.com",
            "https://",
        ] {
            let err = cfg(bad).to_stored_string().expect_err(bad);
            assert!(
                matches!(err, SecretError::InvalidConfig(_)),
                "{bad}: {err:?}"
            );
        }
        for ok in [
            "https://openrouter.ai/api/v1",
            "http://localhost:11434/v1",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        ] {
            cfg(ok).to_stored_string().expect(ok);
        }
        // No override → nothing to validate.
        let none = ProviderConfig {
            base_url: None,
            ..cfg("https://x.example")
        };
        none.to_stored_string().expect("no override is valid");
    }

    /// A previously-stored insecure URL still loads (so the UI can show and
    /// fix it), but the transport refuses to send — fail closed.
    #[test]
    fn stored_insecure_base_url_fails_closed_at_send() {
        let store = InMemorySecretStore::new();
        store
            .set_key(
                r#"{"provider":"anthropic","api_key":"sk-x","base_url":"http://attacker.example"}"#,
            )
            .expect("set");
        let client = client_from_secret(&store, ClientConfig::default()).expect("client");
        let req = crate::client::Request {
            model: "m".to_string(),
            max_tokens: 1,
            system: vec![],
            messages: vec![],
            tools: vec![],
            tool_choice: None,
            thinking: crate::client::ThinkingConfig::adaptive(),
            stream: true,
        };
        let err = client.send(&req).expect_err("must refuse");
        assert!(
            err.to_string().contains("invalid base URL"),
            "unexpected error: {err}"
        );
    }

    /// The OS credential-store backend must compile and implement
    /// SecretStore. This constructs the backend (no credential-store access)
    /// and confirms the trait is satisfied. It runs only where a backend is
    /// compiled in.
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    #[test]
    fn keychain_backend_constructs_and_impls_secret_store() {
        let store = KeychainSecretStore::new();
        // Compile-time check: it's a SecretStore.
        let _store: &dyn SecretStore = &store;
    }
}
