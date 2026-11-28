import { onCleanup, onMount, type ComponentProps } from "solid-js";
import CaptureNote from "../components/CaptureNote/CaptureNote";
import { applyThemeMirror, THEME_MIRROR_KEY } from "../lib/theme-mirror";

/** Paints the theme the main window last saved. */
export function applySavedTheme(): void {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(THEME_MIRROR_KEY);
  } catch {
    return;
  }
  applyThemeMirror(document.documentElement, raw);
}

/**
 * The capture window's page (ADR-055). The main window saves the theme as it
 * changes; the note picks it up each time it is shown.
 */
export default function CaptureWindow(props: ComponentProps<typeof CaptureNote>) {
  onMount(() => {
    window.addEventListener("focus", applySavedTheme);
    onCleanup(() => window.removeEventListener("focus", applySavedTheme));
  });
  return <CaptureNote note={props.note} />;
}
