// Dark/light theme hook.
//
// Persists the choice to localStorage and reflects it on `<html>` as
// `data-theme="dark"|"light"`, which the Ember tokens (styles/tokens.css)
// key off of. Defaults to dark — the comfortable theme for CAD work.
import { useCallback, useEffect, useState } from "react";

export type Theme = "dark" | "light";

export const THEME_STORAGE_KEY = "morphe-theme";

export function useTheme(): { theme: Theme; toggle: () => void } {
  const [theme, setTheme] = useState<Theme>(() => readTheme());

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
    try {
      localStorage.setItem(THEME_STORAGE_KEY, theme);
    } catch {
      // localStorage may be unavailable (private mode / test sandbox) — the
      // in-memory theme still applies for the session.
    }
  }, [theme]);

  const toggle = useCallback(() => {
    setTheme((t) => (t === "dark" ? "light" : "dark"));
  }, []);

  return { theme, toggle };
}

function readTheme(): Theme {
  try {
    const saved = localStorage.getItem(THEME_STORAGE_KEY);
    if (saved === "light" || saved === "dark") return saved;
  } catch {
    // ignore — fall through to default
  }
  return "dark";
}
