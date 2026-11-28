import { describe, it, expect, vi, beforeEach } from "vitest";
import { createRoot } from "solid-js";

const h = vi.hoisted(() => ({
  launchAtLoginStatus: vi.fn(),
  setLaunchAtLogin: vi.fn(),
  showToast: vi.fn(),
}));

vi.mock("../../services/tauri", () => ({
  launchAtLoginStatus: h.launchAtLoginStatus,
  setLaunchAtLogin: h.setLaunchAtLogin,
}));
vi.mock("../../stores/global/notifications", () => ({ showToast: h.showToast }));

import { createLoginItemStore } from "../../stores/global/login-item";

function store() {
  return createRoot(() => createLoginItemStore());
}

describe("open at login", () => {
  beforeEach(() => {
    h.launchAtLoginStatus.mockReset();
    h.setLaunchAtLogin.mockReset();
    h.showToast.mockReset();
  });

  it("reads what the OS has", async () => {
    const login = store();
    h.launchAtLoginStatus.mockResolvedValue({ enabled: true });
    await login.load();
    expect(login.enabled()).toBe(true);
  });

  it("takes the OS's answer on a change", async () => {
    const login = store();
    h.setLaunchAtLogin.mockResolvedValue({ enabled: true });
    await login.set(true);
    expect(h.setLaunchAtLogin).toHaveBeenCalledWith(true);
    expect(login.enabled()).toBe(true);
    expect(login.busy()).toBe(false);
  });

  it("leaves the switch where it was, and says so, when the OS refuses", async () => {
    const login = store();
    h.launchAtLoginStatus.mockResolvedValue({ enabled: false });
    await login.load();
    h.setLaunchAtLogin.mockRejectedValue(new Error("ERR_LOGIN_ITEM: not permitted"));

    await login.set(true);

    expect(login.enabled()).toBe(false);
    expect(h.showToast).toHaveBeenCalledWith("Couldn't add Jotra to your login items", "error");
    expect(login.busy()).toBe(false);
  });

  it("shows the entry the OS made when the settings could not record it", async () => {
    const login = store();
    h.launchAtLoginStatus.mockResolvedValue({ enabled: false });
    await login.load();
    h.setLaunchAtLogin.mockRejectedValue(
      new Error("ERR_LOGIN_ITEM_UNSAVED: the login item changed, but config.toml could not record it"),
    );

    await login.set(true);

    expect(login.enabled()).toBe(true);
    expect(h.showToast).toHaveBeenCalledWith("Could not save your settings", "error");
  });
});
