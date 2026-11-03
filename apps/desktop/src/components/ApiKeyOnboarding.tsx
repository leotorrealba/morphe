// Provider-credential onboarding (provider-agnostic).
//
// On mount, reads the stored provider config (WITHOUT the key — has_key only).
// If none is set, shows the onboarding form: provider select + paste-key field
// + (for OpenAI-compatible) optional base_url/model overrides →
// `setProviderConfig`; if set, shows a status view (provider + effective
// model, key masked) + remove button → `removeApiKey`. Notifies the parent via
// `onKeyChange` whenever the key state flips so the parent can swap to the
// chat surface.
import { useEffect, useState } from "react";

import {
  getProviderConfig,
  providerDefaults,
  removeApiKey,
  setProviderConfig,
  type Provider,
} from "../ai";

export interface ApiKeyOnboardingProps {
  /**
   * Called with the new has-key state whenever the key is set or removed.
   * The parent uses this to gate the chat surface.
   */
  onKeyChange?: (hasKey: boolean) => void;
}

const PROVIDER_LABELS: Record<Provider, string> = {
  anthropic: "Anthropic (Claude)",
  open_ai_compatible: "OpenAI-compatible (OpenAI, OpenRouter, Azure, local…)",
};

export default function ApiKeyOnboarding({ onKeyChange }: ApiKeyOnboardingProps) {
  // null = checking; true = credential present; false = none.
  const [hasKey, setHasKey] = useState<boolean | null>(null);
  // The status view shows the STORED config (provider + effective model +
  // effective base_url when overridden, so a tampered base_url entry is
  // user-visible).
  const [stored, setStored] = useState<{
    provider: Provider;
    model: string | null;
    baseUrl: string | null;
  } | null>(null);
  // Form state.
  const [provider, setProvider] = useState<Provider>("anthropic");
  const [input, setInput] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [model, setModel] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getProviderConfig().then((info) => {
      if (cancelled) return;
      setHasKey(info.has_key);
      setStored({ provider: info.provider, model: info.model, baseUrl: info.base_url });
      setProvider(info.provider);
      setBaseUrl(info.base_url ?? "");
      setModel(info.model ?? "");
    }).catch(() => {
      if (cancelled) return;
      setHasKey(false);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const onSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    const key = input.trim();
    if (!key) {
      setError("Paste your API key first.");
      return;
    }
    setError(null);
    try {
      await setProviderConfig(
        provider,
        key,
        baseUrl.trim() || null,
        model.trim() || null,
      );
      setStored({
        provider,
        model: model.trim() || null,
        baseUrl: baseUrl.trim() || null,
      });
      setHasKey(true);
      setInput("");
      onKeyChange?.(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const onRemove = async () => {
    try {
      await removeApiKey();
      setHasKey(false);
      onKeyChange?.(false);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  if (hasKey === null) {
    return (
      <div data-testid="apikey-loading" style={paneStyle}>
        Checking API key…
      </div>
    );
  }

  if (hasKey) {
    const effectiveModel =
      stored?.model ?? providerDefaults[stored?.provider ?? "anthropic"].model;
    // Show the effective base_url when overridden, so a tampered/stale entry
    // is visible to the user.
    const effectiveBaseUrl = stored?.baseUrl ?? null;
    return (
      <div data-testid="apikey-present" style={paneStyle}>
        <p style={{ margin: 0, color: "var(--morphe-text)" }}>
          API key set <span style={{ color: "var(--morphe-success)" }}>●</span>
        </p>
        <p style={{ margin: "var(--morphe-space-1) 0", color: "var(--morphe-text-dim)", fontSize: "var(--morphe-font-size-sm)" }}>
          {PROVIDER_LABELS[stored?.provider ?? "anthropic"]} ·{" "}
          <span data-testid="apikey-model-display">{effectiveModel}</span> ·
          key masked
        </p>
        {effectiveBaseUrl && (
          <p
            data-testid="apikey-baseurl-display"
            style={{ margin: 0, color: "var(--morphe-text-dim)", fontSize: "var(--morphe-font-size-sm)", fontFamily: "var(--morphe-font-mono)" }}
          >
            → {effectiveBaseUrl}
          </p>
        )}
        <button
          type="button"
          data-testid="apikey-remove"
          onClick={onRemove}
          style={removeBtnStyle}
        >
          Remove key
        </button>
        {error && (
          <p data-testid="apikey-error" style={{ color: "var(--morphe-error)", fontSize: "var(--morphe-font-size-sm)" }}>
            {error}
          </p>
        )}
      </div>
    );
  }

  const defaults = providerDefaults[provider].model;
  const defaultUrl = providerDefaults[provider].baseUrl;

  return (
    <form data-testid="apikey-form" onSubmit={onSubmit} style={paneStyle}>
      <p style={{ margin: 0, color: "var(--morphe-text)", fontWeight: 600 }}>
        Add your API key
      </p>
      <p style={{ margin: "var(--morphe-space-1) 0", color: "var(--morphe-text-dim)", fontSize: "var(--morphe-font-size-sm)" }}>
        Stored in the OS keychain. Required to run the agent.
      </p>
      <select
        data-testid="apikey-provider"
        value={provider}
        onChange={(e) => {
          const next = e.target.value as Provider;
          setProvider(next);
          // Switching away from openai_compatible clears the overrides — the
          // fields are hidden but stale values would be stored invisibly
          // (clear, don't carry hidden state).
          if (next !== "open_ai_compatible") {
            setBaseUrl("");
            setModel("");
          }
        }}
        style={inputStyle}
      >
        {Object.entries(PROVIDER_LABELS).map(([value, label]) => (
          <option key={value} value={value}>
            {label}
          </option>
        ))}
      </select>
      <input
        type="password"
        data-testid="apikey-input"
        placeholder={
          provider === "anthropic"
            ? "paste Anthropic API key"
            : "paste OpenAI-compatible API key"
        }
        value={input}
        onChange={(e) => setInput(e.target.value)}
        style={inputStyle}
        autoComplete="off"
      />
      {provider === "open_ai_compatible" && (
        <>
          <input
            type="text"
            data-testid="apikey-baseurl"
            placeholder={`base URL (default ${defaultUrl})`}
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            style={inputStyle}
            autoComplete="off"
            spellCheck={false}
          />
          <input
            type="text"
            data-testid="apikey-model"
            placeholder={`model id (default ${defaults})`}
            value={model}
            onChange={(e) => setModel(e.target.value)}
            style={inputStyle}
            autoComplete="off"
            spellCheck={false}
          />
        </>
      )}
      <button type="submit" data-testid="apikey-submit" style={submitBtnStyle}>
        Save key
      </button>
      {error && (
        <p data-testid="apikey-error" style={{ color: "var(--morphe-error)", fontSize: "var(--morphe-font-size-sm)" }}>
          {error}
        </p>
      )}
    </form>
  );
}


const paneStyle: React.CSSProperties = {
  padding: "var(--morphe-space-4)",
  display: "flex",
  flexDirection: "column",
  gap: "var(--morphe-space-2)",
};
const inputStyle: React.CSSProperties = {
  background: "var(--morphe-bg)",
  color: "var(--morphe-text)",
  border: "1px solid var(--morphe-border)",
  borderRadius: "var(--morphe-radius-sm)",
  padding: "var(--morphe-space-2)",
  fontFamily: "var(--morphe-font-mono)",
  fontSize: "var(--morphe-font-size-sm)",
};
const submitBtnStyle: React.CSSProperties = {
  background: "var(--morphe-accent)",
  color: "#1a1a1c",
  border: "none",
  borderRadius: "var(--morphe-radius-sm)",
  padding: "var(--morphe-space-2) var(--morphe-space-3)",
  cursor: "pointer",
  fontWeight: 600,
};
const removeBtnStyle: React.CSSProperties = {
  background: "var(--morphe-surface-2)",
  color: "var(--morphe-text)",
  border: "1px solid var(--morphe-border)",
  borderRadius: "var(--morphe-radius-sm)",
  padding: "var(--morphe-space-1) var(--morphe-space-3)",
  cursor: "pointer",
};
