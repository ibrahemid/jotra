import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup } from "@solidjs/testing-library";
import { createSignal, Show } from "solid-js";
import type { BufferDocument } from "../../types/buffer";
import WindowProvider from "../../components/WindowProvider/WindowProvider";
import { windowRegistry } from "../../stores/global/window-registry";

// An editor that goes away while its note is still being read must not put a
// view into its pane once the read lands.

const h = vi.hoisted(() => ({ answer: (_text: string) => {} }));

vi.mock("../../services/tauri", () => ({
  readBufferContent: vi.fn(async () => ""),
  saveBufferContent: vi.fn(async () => {}),
}));

vi.mock("../../services/events", () => ({
  onEvent: vi.fn(async () => () => {}),
  emitFrontendReady: vi.fn(async () => {}),
}));

vi.mock("../../stores/global/buffer-registry", () => ({
  bufferRegistry: {
    readContent: vi.fn(() => new Promise<string>((resolve) => (h.answer = resolve))),
  },
}));

function note(id: string): BufferDocument {
  return {
    id,
    title: `${id}.md`,
    filename: `${id}.md`,
    status: "active",
    language: null,
    source_path: `/notes/${id}.md`,
    cursor_pos: 0,
    scroll_pos: 0,
    tab_order: 0,
    created_at: "2026-11-01T09:00:00Z",
    updated_at: "2026-11-01T09:00:00Z",
    closed_at: null,
    read_only: false,
    size_bytes: 0,
    line_ending: "lf",
    encoding: "UTF-8",
  };
}

describe("EditorInstance unmounted during its read", () => {
  afterEach(() => cleanup());

  it("leaves its pane without a view when the read lands later", async () => {
    const { readContent } = (await import("../../stores/global/buffer-registry")).bufferRegistry;
    const EditorInstance = (await import("../../components/Editor/EditorInstance")).default;
    const [shown, setShown] = createSignal(true);
    const { container } = render(() => (
      <WindowProvider windowId={9611}>
        <Show when={shown()}>
          <EditorInstance buffer={note("N1")} />
        </Show>
      </WindowProvider>
    ));
    await vi.waitFor(() => expect(readContent).toHaveBeenCalled());

    setShown(false);
    h.answer("text that arrived late");
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(windowRegistry.getActive()?.editor.liveDoc()).toBeNull();
    expect(container.querySelector(".cm-editor")).toBeNull();
  });
});
