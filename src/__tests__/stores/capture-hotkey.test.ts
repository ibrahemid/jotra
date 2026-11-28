import { describe, it, expect, vi, beforeEach } from "vitest";
import { createRoot } from "solid-js";

const h = vi.hoisted(() => ({
  captureHotkeyStatus: vi.fn(),
  setCaptureHotkey: vi.fn(),
  onEvent: vi.fn(),
}));

vi.mock("../../services/tauri", () => ({
  captureHotkeyStatus: h.captureHotkeyStatus,
  setCaptureHotkey: h.setCaptureHotkey,
}));
vi.mock("../../services/events", () => ({ onEvent: h.onEvent }));

import { createCaptureHotkeyStore } from "../../stores/global/hotkey";

function store() {
  return createRoot(() => createCaptureHotkeyStore());
}

describe("the capture chord's store", () => {
  beforeEach(() => {
    h.captureHotkeyStatus.mockReset();
    h.setCaptureHotkey.mockReset();
    h.onEvent.mockReset();
  });

  it("listens on its own event, apart from the window chord's", async () => {
    h.onEvent.mockResolvedValue(() => {});
    await store().subscribe();
    expect(h.onEvent).toHaveBeenCalledWith("hotkey:capture-status", expect.any(Function));
  });

  it("reads a capture chord another app holds as taken", async () => {
    h.captureHotkeyStatus.mockResolvedValue({
      chord: "CmdOrCtrl+Shift+Alt+Space",
      registered: false,
      unsupported: false,
    });
    const capture = store();
    await capture.load();
    expect(capture.isTaken()).toBe(true);
  });

  it("reads a capture chord the window's chord holds as held by it, not taken", async () => {
    h.captureHotkeyStatus.mockResolvedValue({
      chord: "CmdOrCtrl+Shift+Space",
      registered: false,
      unsupported: false,
    });
    const windowHeld = { chord: () => "Shift+CmdOrCtrl+Space", isHeld: () => true };
    const capture = createRoot(() => createCaptureHotkeyStore(() => windowHeld));
    await capture.load();
    expect(capture.isHeldByOther()).toBe(true);
    expect(capture.isTaken()).toBe(false);
  });

  it("reads a capture chord spelled another way as held by the window on macOS", async () => {
    h.captureHotkeyStatus.mockResolvedValue({
      chord: "cmd+shift+space",
      registered: false,
      unsupported: false,
    });
    const windowHeld = { chord: () => "CmdOrCtrl+Shift+Space", isHeld: () => true };
    const capture = createRoot(() => createCaptureHotkeyStore(() => windowHeld, "mac"));
    await capture.load();
    expect(capture.isHeldByOther()).toBe(true);
    expect(capture.isTaken()).toBe(false);
  });

  it("reads Ctrl as the window's CmdOrCtrl only where the OS holds them as one key", async () => {
    h.captureHotkeyStatus.mockResolvedValue({
      chord: "Ctrl+Shift+Space",
      registered: false,
      unsupported: false,
    });
    const windowHeld = { chord: () => "CmdOrCtrl+Shift+Space", isHeld: () => true };
    const onWindows = createRoot(() => createCaptureHotkeyStore(() => windowHeld, "win"));
    await onWindows.load();
    expect(onWindows.isHeldByOther()).toBe(true);
    const onMac = createRoot(() => createCaptureHotkeyStore(() => windowHeld, "mac"));
    await onMac.load();
    expect(onMac.isHeldByOther()).toBe(false);
    expect(onMac.isTaken()).toBe(true);
  });

  it("reads the window's keys as taken when the window's chord is not held either", async () => {
    h.captureHotkeyStatus.mockResolvedValue({
      chord: "CmdOrCtrl+Shift+Space",
      registered: false,
      unsupported: false,
    });
    const windowUnheld = { chord: () => "CmdOrCtrl+Shift+Space", isHeld: () => false };
    const capture = createRoot(() => createCaptureHotkeyStore(() => windowUnheld));
    await capture.load();
    expect(capture.isHeldByOther()).toBe(false);
    expect(capture.isTaken()).toBe(true);
  });

  it("reads capture turned off as off, not taken", async () => {
    h.captureHotkeyStatus.mockResolvedValue({ chord: "", registered: false, unsupported: false });
    const capture = store();
    await capture.load();
    expect(capture.chord()).toBe("");
    expect(capture.isTaken()).toBe(false);
  });

  it("asks for the capture chord through its own call", async () => {
    h.setCaptureHotkey.mockResolvedValue({
      status: { chord: "Ctrl+Alt+N", registered: true, unsupported: false },
      refused: false,
    });
    const capture = store();
    expect(await capture.rebind("Ctrl+Alt+N")).toBe(true);
    expect(h.setCaptureHotkey).toHaveBeenCalledWith("Ctrl+Alt+N");
    expect(capture.chord()).toBe("Ctrl+Alt+N");
  });
});
