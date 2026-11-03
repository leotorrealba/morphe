// Tests for the provider-agnostic ApiKeyOnboarding flow.
//
// `../../ai` is mocked so the test drives the IPC surface
// (getProviderConfig / setProviderConfig / removeApiKey) without the backend.
// Covers: default render, provider switch reveals override fields, empty-key
// validation, submit payload shape, status view shows provider + effective
// model + effective base_url when overridden (so a tampered base_url is
// visible).
import { expect, test, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

import ApiKeyOnboarding from "../ApiKeyOnboarding";
import {
  getProviderConfig,
  setProviderConfig,
  removeApiKey,
} from "../../ai";

vi.mock("../../ai", () => ({
  getProviderConfig: vi.fn(),
  setProviderConfig: vi.fn(async () => undefined),
  removeApiKey: vi.fn(async () => undefined),
  // The component reads the display-only default mirror; the REAL module
  // exports it (src/ai.ts) — the mock must too or the component crashes.
  providerDefaults: {
    anthropic: { baseUrl: "https://api.anthropic.com", model: "claude-sonnet-5-5" },
    open_ai_compatible: { baseUrl: "https://api.openai.com", model: "gpt-4o" },
  },
}));

beforeEach(() => {
  vi.mocked(getProviderConfig).mockReset();
  vi.mocked(setProviderConfig).mockClear();
  vi.mocked(removeApiKey).mockClear();
});

test("default render: no key, anthropic pre-selected, no override fields", async () => {
  vi.mocked(getProviderConfig).mockResolvedValueOnce({
    provider: "anthropic",
    base_url: null,
    model: null,
    has_key: false,
  });

  render(<ApiKeyOnboarding />);

  const form = await screen.findByTestId("apikey-form");
  expect(form).toBeInTheDocument();
  // Anthropic default: no base_url/model fields.
  expect(screen.getByTestId("apikey-provider")).toHaveValue("anthropic");
  expect(screen.queryByTestId("apikey-baseurl")).not.toBeInTheDocument();
  expect(screen.queryByTestId("apikey-model")).not.toBeInTheDocument();
});

test("provider switch to openai_compatible reveals base_url/model fields", async () => {
  vi.mocked(getProviderConfig).mockResolvedValueOnce({
    provider: "anthropic",
    base_url: null,
    model: null,
    has_key: false,
  });

  render(<ApiKeyOnboarding />);
  await screen.findByTestId("apikey-form");

  fireEvent.change(screen.getByTestId("apikey-provider"), {
    target: { value: "open_ai_compatible" },
  });

  expect(screen.getByTestId("apikey-baseurl")).toBeInTheDocument();
  expect(screen.getByTestId("apikey-model")).toBeInTheDocument();
});

test("empty key: validation error, no IPC call", async () => {
  vi.mocked(getProviderConfig).mockResolvedValueOnce({
    provider: "anthropic",
    base_url: null,
    model: null,
    has_key: false,
  });

  render(<ApiKeyOnboarding />);
  await screen.findByTestId("apikey-form");

  fireEvent.click(screen.getByTestId("apikey-submit"));

  expect(await screen.findByTestId("apikey-error")).toBeInTheDocument();
  expect(setProviderConfig).not.toHaveBeenCalled();
});

test("submit: typed payload with trimmed optional fields → null", async () => {
  vi.mocked(getProviderConfig).mockResolvedValueOnce({
    provider: "anthropic",
    base_url: null,
    model: null,
    has_key: false,
  });

  render(<ApiKeyOnboarding />);
  await screen.findByTestId("apikey-form");

  // Choose OpenAI-compatible and fill key + overrides.
  fireEvent.change(screen.getByTestId("apikey-provider"), {
    target: { value: "open_ai_compatible" },
  });
  fireEvent.change(screen.getByTestId("apikey-input"), {
    target: { value: "sk-oai-fixture" },
  });
  fireEvent.change(screen.getByTestId("apikey-baseurl"), {
    target: { value: "https://openrouter.example/api  " },
  });
  fireEvent.change(screen.getByTestId("apikey-model"), {
    target: { value: "  " },
  });
  fireEvent.click(screen.getByTestId("apikey-submit"));

  // Trailing/whitespace-only fields normalize: trimmed URL kept, blank model
  // → null (provider default).
  await waitFor(() =>
    expect(setProviderConfig).toHaveBeenCalledWith(
      "open_ai_compatible",
      "sk-oai-fixture",
      "https://openrouter.example/api",
      null,
    ),
  );

  // Surface flips to the status view.
  await waitFor(() =>
    expect(screen.getByTestId("apikey-present")).toBeInTheDocument(),
  );
  // Effective model resolves client-side to the provider default (blank input).
  expect(screen.getByTestId("apikey-model-display")).toHaveTextContent(
    "gpt-4o",
  );
  // The effective base_url is visible when overridden.
  expect(screen.getByTestId("apikey-baseurl-display")).toHaveTextContent(
    "https://openrouter.example/api",
  );
});

test("set → remove → back to form; backward-compat bare key reads as anthropic", async () => {
  // Legacy bare key (backward compat): reads as Anthropic + defaults.
  vi.mocked(getProviderConfig).mockResolvedValueOnce({
    provider: "anthropic",
    base_url: null,
    model: null,
    has_key: true,
  });

  render(<ApiKeyOnboarding />);

  const status = await screen.findByTestId("apikey-present");
  expect(status).toBeInTheDocument();
  expect(screen.getByTestId("apikey-model-display")).toHaveTextContent(
    "claude-sonnet-5-5",
  );
  // No override → no base_url display.
  expect(
    screen.queryByTestId("apikey-baseurl-display"),
  ).not.toBeInTheDocument();

  // Remove → back to the form.
  fireEvent.click(screen.getByTestId("apikey-remove"));
  await waitFor(() => expect(removeApiKey).toHaveBeenCalled());
  await waitFor(() =>
    expect(screen.getByTestId("apikey-form")).toBeInTheDocument(),
  );
});
