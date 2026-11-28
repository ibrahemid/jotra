import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, cleanup, fireEvent } from "@solidjs/testing-library";

beforeAll(() => {
  Object.defineProperty(window, "matchMedia", {
    writable: true,
    value: vi.fn().mockImplementation((query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })),
  });
  if (!Element.prototype.scrollIntoView) {
    Element.prototype.scrollIntoView = vi.fn();
  }
});

const mocks = vi.hoisted(() => ({
  // The strip is hidden at one note, so the close control needs two.
  activeTabs: vi.fn(() => [
    { id: "buf-1", title: "alpha.md", filename: "alpha.md", source_path: null },
    { id: "buf-2", title: "beta.md", filename: "beta.md", source_path: null },
  ]),
  activeTabId: vi.fn(() => "buf-1"),
  setActiveTabId: vi.fn(),
  closeTab: vi.fn(),
  closeOtherTabs: vi.fn(),
  closeAllTabs: vi.fn(),
  createTab: vi.fn(),
  renameBuffer: vi.fn(() => Promise.resolve()),
  showContextMenu: vi.fn(),
  focusEditor: vi.fn(),
  moveToOtherPane: vi.fn(),
}));

vi.mock("../../components/WindowProvider/WindowProvider", () => ({
  useWindow: () => ({
    panes: {
      holds: () => true,
      activeIn: () => mocks.activeTabId(),
      isSplit: () => true,
      focused: () => "left",
      focus: () => {},
    },
    tabs: {
      selectIn: (_pane: string, id: string) => mocks.setActiveTabId(id),
      showDownloadPane: () => mocks.setActiveTabId(null),
      moveToOtherPane: mocks.moveToOtherPane,
      activeTabId: mocks.activeTabId,
      setActiveTabId: mocks.setActiveTabId,
      closeTab: mocks.closeTab,
      closeOtherTabs: mocks.closeOtherTabs,
      closeAllTabs: mocks.closeAllTabs,
      createTab: mocks.createTab,
    },
    editor: { focusEditor: mocks.focusEditor, isRemovedOnDisk: () => false },
    // No note is waiting on a sync provider in these cases.
    downloads: {
      pending: () => [],
      selectedPath: () => null,
      select: () => {},
      dismiss: async () => {},
    },
  }),
}));

vi.mock("../../stores/global/buffer-registry", () => ({
  bufferRegistry: {
    activeTabs: mocks.activeTabs,
    renameBuffer: mocks.renameBuffer,
  },
}));

vi.mock("../../stores/global/window-registry", () => ({
  windowRegistry: {
    getActive: () => ({
      tabs: {
        activeTabId: mocks.activeTabId,
        closeTab: mocks.closeTab,
      },
    }),
  },
}));

vi.mock("../../components/ContextMenu/ContextMenu", () => ({
  showContextMenu: mocks.showContextMenu,
}));

import TabBar from "../../components/Editor/TabBar";
import { registerStrip } from "../../components/Editor/tab-drag";

// A tab dragged onto the other pane's strip leaves no click behind, so the
// next tab chosen from the keyboard on this strip is still chosen.

function stripAt(left: number): HTMLElement {
  const el = document.createElement("div");
  el.getBoundingClientRect = () => ({ left, right: left + 100, top: 0, bottom: 30 }) as DOMRect;
  return el;
}

describe("TabBar after a tab is dragged to the other pane", () => {
  afterEach(() => {
    mocks.setActiveTabId.mockClear();
    cleanup();
  });

  it("selects the next tab picked with Enter or Space", () => {
    const stopRight = registerStrip("right", stripAt(500));
    const { container } = render(() => <TabBar />);
    const [first, second] = [...container.querySelectorAll<HTMLButtonElement>(".tab-label")];
    first.setPointerCapture = () => {};

    fireEvent.pointerDown(first, { button: 0, pointerId: 1, clientX: 10, clientY: 10 });
    fireEvent.pointerMove(first, { pointerId: 1, clientX: 520, clientY: 10 });
    fireEvent.pointerUp(first, { pointerId: 1, clientX: 520, clientY: 10 });
    expect(mocks.moveToOtherPane).toHaveBeenCalledWith("buf-1", "left");

    fireEvent.click(second, { detail: 0 });
    expect(mocks.setActiveTabId).toHaveBeenCalledWith("buf-2");
    stopRight();
  });

  it("still swallows the click that ends a drag on its own strip", () => {
    const { container } = render(() => <TabBar />);
    const [first] = [...container.querySelectorAll<HTMLButtonElement>(".tab-label")];
    first.setPointerCapture = () => {};

    fireEvent.pointerDown(first, { button: 0, pointerId: 1, clientX: 10, clientY: 10 });
    fireEvent.pointerMove(first, { pointerId: 1, clientX: 60, clientY: 10 });
    fireEvent.pointerUp(first, { pointerId: 1, clientX: 60, clientY: 10 });
    fireEvent.click(first, { detail: 1 });
    expect(mocks.setActiveTabId).not.toHaveBeenCalled();
  });
});
