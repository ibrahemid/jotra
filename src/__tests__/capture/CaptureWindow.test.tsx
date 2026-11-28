import { describe, it, expect, vi, afterEach } from "vitest";
import { createRoot } from "solid-js";
import { render, cleanup } from "@solidjs/testing-library";
import CaptureWindow from "../../capture/CaptureWindow";
import { createCaptureNoteStore } from "../../stores/capture/capture-note";
import { THEME_MIRROR_KEY } from "../../lib/theme-mirror";

function note() {
  return createRoot(() =>
    createCaptureNoteStore({
      append: vi.fn(async () => ({ name: "2026-10-15.txt", added: true })),
      target: vi.fn(async () => "2026-10-15.txt"),
      close: vi.fn(async () => {}),
      draft: vi.fn(async () => ""),
      keep: vi.fn(async () => {}),
    }),
  );
}

function saveTheme(theme: string): void {
  localStorage.setItem(THEME_MIRROR_KEY, JSON.stringify({ attrs: { "data-theme": theme } }));
}

describe("the capture window's page", () => {
  afterEach(() => {
    cleanup();
    localStorage.removeItem(THEME_MIRROR_KEY);
    document.documentElement.removeAttribute("data-theme");
  });

  it("picks up the theme the main window saved when it is shown", () => {
    render(() => <CaptureWindow note={note()} />);
    saveTheme("dark");
    window.dispatchEvent(new Event("focus"));
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
  });

  it("stops listening once the page is gone", () => {
    const view = render(() => <CaptureWindow note={note()} />);
    view.unmount();
    saveTheme("light");
    window.dispatchEvent(new Event("focus"));
    expect(document.documentElement.getAttribute("data-theme")).toBeNull();
  });
});
