import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup, fireEvent } from "@solidjs/testing-library";
import { createSignal } from "solid-js";

// The divider between the panes states and keeps the range the panes hold.

const h = vi.hoisted(() => ({ setRatio: vi.fn() }));

vi.mock("../../components/Editor/EditorInstance", () => ({ default: () => null }));
vi.mock("../../components/Editor/TabBar", () => ({ default: () => null }));
vi.mock("../../components/Editor/StartSurface", () => ({ default: () => null }));
vi.mock("../../components/Editor/NoteDownloading", () => ({ default: () => null }));
vi.mock("../../stores/global/buffer-registry", () => ({ bufferRegistry: { activeTabs: () => [] } }));

const [ratio, setRatioSignal] = createSignal(0.21);
vi.mock("../../components/WindowProvider/WindowProvider", () => ({
  useWindow: () => ({
    panes: {
      isSplit: () => true,
      focused: () => "left",
      activeIn: () => null,
      focus: () => {},
      settle: () => {},
      ratio,
      setRatio: (next: number, commit: boolean) => {
        h.setRatio(next, commit);
        setRatioSignal(next);
      },
    },
    downloads: { selected: () => null },
  }),
}));

import PaneGroup from "../../components/Editor/PaneGroup";
import { MAX_PANE_RATIO, MIN_PANE_RATIO } from "../../lib/pane-layout";

describe("the divider between the panes", () => {
  afterEach(() => cleanup());

  it("announces the range the panes are held to", () => {
    const { container } = render(() => <PaneGroup focusedBuffer={null} />);
    const handle = container.querySelector<HTMLElement>('[role="separator"]')!;
    expect(handle.getAttribute("aria-valuemin")).toBe(String(Math.round(MIN_PANE_RATIO * 100)));
    expect(handle.getAttribute("aria-valuemax")).toBe(String(Math.round(MAX_PANE_RATIO * 100)));
  });

  it("stops a keyboard step at the narrowest pane", () => {
    const { container } = render(() => <PaneGroup focusedBuffer={null} />);
    const handle = container.querySelector<HTMLElement>('[role="separator"]')!;
    fireEvent.keyDown(handle, { key: "ArrowLeft" });
    expect(h.setRatio).toHaveBeenCalledWith(MIN_PANE_RATIO, false);
  });
});
