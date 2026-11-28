import { describe, it, expect, vi, beforeEach } from "vitest";

const h = vi.hoisted(() => ({
  fetchDefaultAppTypes: vi.fn(),
  fetchDefaultAppStatus: vi.fn(),
  fetchDefaultAppSupport: vi.fn(),
}));

vi.mock("../../stores/global/default-app", () => ({
  fetchDefaultAppTypes: h.fetchDefaultAppTypes,
  fetchDefaultAppStatus: h.fetchDefaultAppStatus,
  fetchDefaultAppSupport: h.fetchDefaultAppSupport,
}));

import {
  probeDefaultAppSupport,
  hasSupportedDefaultAppTypes,
  markDefaultAppTypeSupported,
  clearDefaultAppSupport,
} from "../../stores/global/default-app-support";

function type(id: string) {
  return { id, label: id, exts: [id], utis: [`public.${id}`] };
}

describe("default-app support registry", () => {
  beforeEach(() => {
    clearDefaultAppSupport();
    h.fetchDefaultAppTypes.mockReset();
    h.fetchDefaultAppStatus.mockReset();
    h.fetchDefaultAppSupport.mockReset();
  });

  it("marks every type supported from the system answer without reading any default", async () => {
    h.fetchDefaultAppTypes.mockResolvedValue([type("markdown"), type("source-code")]);
    h.fetchDefaultAppSupport.mockResolvedValue(true);

    await probeDefaultAppSupport();
    expect(hasSupportedDefaultAppTypes()).toBe(true);
    expect(h.fetchDefaultAppStatus).not.toHaveBeenCalled();

    markDefaultAppTypeSupported("markdown", false);
    expect(hasSupportedDefaultAppTypes()).toBe(true);
    markDefaultAppTypeSupported("source-code", false);
    expect(hasSupportedDefaultAppTypes()).toBe(false);
  });

  it("marks no type supported on a system that offers no default apps", async () => {
    h.fetchDefaultAppTypes.mockResolvedValue([type("markdown")]);
    h.fetchDefaultAppSupport.mockResolvedValue(false);
    await probeDefaultAppSupport();
    expect(hasSupportedDefaultAppTypes()).toBe(false);
  });

  it("leaves the registry empty when listing types fails", async () => {
    h.fetchDefaultAppTypes.mockRejectedValue(new Error("no IPC"));
    h.fetchDefaultAppSupport.mockResolvedValue(true);
    await probeDefaultAppSupport();
    expect(hasSupportedDefaultAppTypes()).toBe(false);
  });

  it("leaves the registry empty when the support check fails", async () => {
    h.fetchDefaultAppTypes.mockResolvedValue([type("markdown")]);
    h.fetchDefaultAppSupport.mockRejectedValue(new Error("boom"));
    await probeDefaultAppSupport();
    expect(hasSupportedDefaultAppTypes()).toBe(false);
  });

  it("updates support imperatively", () => {
    markDefaultAppTypeSupported("config-data", true);
    expect(hasSupportedDefaultAppTypes()).toBe(true);
    markDefaultAppTypeSupported("config-data", false);
    expect(hasSupportedDefaultAppTypes()).toBe(false);
  });
});
