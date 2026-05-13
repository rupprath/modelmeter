import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";

export type ThemePref = "light" | "dark" | "system";

export const THEME_EVENT = "mm-theme-changed";

export function applyThemePref(pref: ThemePref) {
  if (pref === "light" || pref === "dark") {
    document.documentElement.setAttribute("data-mm-theme", pref);
  } else {
    const dark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    document.documentElement.setAttribute("data-mm-theme", dark ? "dark" : "light");
  }
}

export function emitThemeChanged(pref: ThemePref): Promise<void> {
  return emit(THEME_EVENT, pref);
}

export function onThemeChanged(handler: (pref: ThemePref) => void): Promise<UnlistenFn> {
  return listen<ThemePref>(THEME_EVENT, (e) => handler(e.payload));
}
