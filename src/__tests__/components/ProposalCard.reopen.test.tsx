import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, cleanup, fireEvent, waitFor } from "@solidjs/testing-library";

// A card read back from a stored conversation, and the listeners that keep an
// open offer measured: what the store recorded of an apply decides Undo, a
// file outside the notes folder waits for its tab, and a closed pane reads
// nothing.

type Handler = (payload: unknown) => void;
type SavedListener = (bufferId: string, diskHash: string | null) => void;
type Tab = { id: string; source_path: string | null };

const mocks = vi.hoisted(() => ({
  chatList: vi.fn(),
  chatOpen: vi.fn(),
  chatRenderReply: vi.fn(),
  chatApplyProposal: vi.fn(),
  chatDiscardProposal: vi.fn(),
  chatAttachedSizes: vi.fn(),
  chatUndoProposal: vi.fn(),
  openFile: vi.fn(),
}));

const bus = vi.hoisted(() => ({
  handlers: new Map<string, Handler[]>(),
  saved: [] as SavedListener[],
  setTabs: (_tabs: Tab[]) => undefined as void,
}));

vi.mock("../../services/tauri", () => ({
  writeClipboardText: vi.fn().mockResolvedValue(undefined),
  chatState: vi.fn(),
  chatAttachedSizes: mocks.chatAttachedSizes,
  chatList: mocks.chatList,
  chatOpen: mocks.chatOpen,
  chatNew: vi.fn(),
  chatRename: vi.fn(),
  chatDelete: vi.fn(),
  chatRenderReply: mocks.chatRenderReply,
  chatSend: vi.fn(),
  chatStop: vi.fn(),
  chatApplyProposal: mocks.chatApplyProposal,
  chatDiscardProposal: mocks.chatDiscardProposal,
  chatUndoProposal: mocks.chatUndoProposal,
}));

vi.mock("../../services/events", () => ({
  onEvent: (kind: string, handler: Handler) => {
    bus.handlers.set(kind, [...(bus.handlers.get(kind) ?? []), handler]);
    return Promise.resolve(() => {
      bus.handlers.set(
        kind,
        (bus.handlers.get(kind) ?? []).filter((held) => held !== handler),
      );
    });
  },
}));

vi.mock("../../services/autosave", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/autosave")>()),
  onAutosaveSuccess: (listener: SavedListener) => {
    bus.saved.push(listener);
    return () => {
      bus.saved = bus.saved.filter((held) => held !== listener);
    };
  },
}));

vi.mock("../../stores/global/buffer-registry", async () => {
  const { createSignal } = await import("solid-js");
  const [tabs, setTabs] = createSignal<Tab[]>([]);
  bus.setTabs = (next) => void setTabs(next);
  return { bufferRegistry: { activeTabs: tabs } };
});

vi.mock("../../stores/global/window-registry", () => ({
  windowRegistry: {
    getActive: () => ({ tabs: { openFile: mocks.openFile }, chatPanel: { show: vi.fn() } }),
  },
}));

vi.mock("../../stores/global/notes", () => ({
  notesStore: { root: () => "/notes", contains: () => true },
}));

import ProposalCard from "../../components/Chat/ProposalCard";
import { chatStore } from "../../stores/global/chat";
import type { ChatConversation, ChatProposal } from "../../services/tauri";

const OUTSIDE = "/elsewhere/repo/README.md";

const PROPOSAL: ChatProposal = {
  path: "Launch.md",
  before_hash: "abc",
  new_content: "Newer text\n",
  summary: "Tighten the opening",
  hunks: [{ before_start: 1, after_start: 1, lines: [{ kind: "added", text: "Newer" }] }],
  status: "pending",
  stale: false,
};

const APPLIED: Partial<ChatProposal> = {
  status: "applied",
  hunks: [],
  applied_hash: "def",
  undone: false,
  undoable: true,
};

function fileWith(offer: Partial<ChatProposal> = {}): ChatConversation {
  return {
    id: "c1",
    title: "A chat",
    created_at: "",
    updated_at: "",
    provider: "ollama",
    model: "a-model",
    turns: [
      { role: "user", content: "tighten it", attachments: [], proposals: [] },
      {
        role: "assistant",
        content: "here",
        attachments: [],
        proposals: [{ ...PROPOSAL, ...offer }],
      },
    ],
  };
}

function emit(kind: string, payload: unknown) {
  for (const handler of bus.handlers.get(kind) ?? []) handler(payload);
}

function saveTab(id: string) {
  for (const listener of [...bus.saved]) listener(id, "new-hash");
}

async function flush() {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

async function openCard(offer: Partial<ChatProposal> = {}) {
  mocks.chatOpen.mockResolvedValue(fileWith(offer));
  await chatStore.openPane();
  const view = render(() => (
    <ProposalCard turn={1} proposal={chatStore.messages()[1].proposals[0]} />
  ));
  const verdict = () => view.container.querySelector(".chat-proposal-verdict")?.textContent;
  const apply = () => view.getByRole("button", { name: "Apply" }) as HTMLButtonElement;
  return { ...view, verdict, apply };
}

beforeEach(() => {
  for (const mock of Object.values(mocks)) mock.mockReset();
  bus.setTabs([{ id: "b1", source_path: "/notes/Launch.md" }]);
  mocks.chatList.mockResolvedValue([
    { id: "c1", title: "A chat", created_at: "", updated_at: "", turns: 2 },
  ]);
  mocks.chatRenderReply.mockImplementation(async (text: string) => `<p>${text}</p>`);
  mocks.chatAttachedSizes.mockResolvedValue([]);
  mocks.chatUndoProposal.mockResolvedValue({
    path: "Launch.md",
    hash: "abc",
    bytes: 12,
    changed: true,
    undoable: false,
  });
  chatStore.reset();
  chatStore.setDraft("");
});

afterEach(async () => {
  cleanup();
  chatStore.closePane();
  await flush();
});

describe("a card read back from a stored conversation", () => {
  it("offers Undo for a write whose earlier text is kept, and undoes the stored write", async () => {
    const { verdict, getByRole, queryByRole } = await openCard(APPLIED);
    expect(verdict()).toBe("Applied.");

    fireEvent.click(getByRole("button", { name: "Undo" }));

    await waitFor(() =>
      expect(mocks.chatUndoProposal).toHaveBeenCalledWith("c1", 1, "Launch.md", "abc", "def"),
    );
    await waitFor(() => expect(verdict()).toBe("Launch.md is back as it was."));
    expect(queryByRole("button", { name: "Undo" })).toBeNull();
  });

  it("says a write that was undone is back as it was, with no Undo", async () => {
    const { verdict, queryByRole } = await openCard({
      ...APPLIED,
      undone: true,
      undoable: false,
    });
    expect(verdict()).toBe("Launch.md is back as it was.");
    expect(queryByRole("button", { name: "Undo" })).toBeNull();
    expect(queryByRole("button", { name: "Open" })).not.toBeNull();
  });

  it("offers Open only once the earlier text is gone", async () => {
    const { verdict, queryByRole } = await openCard({ ...APPLIED, undoable: false });
    expect(verdict()).toBe("Applied.");
    expect(queryByRole("button", { name: "Undo" })).toBeNull();
    expect(queryByRole("button", { name: "Open" })).not.toBeNull();
  });

  it("offers Undo for a file outside the notes folder while its earlier text is kept", async () => {
    mocks.chatUndoProposal.mockResolvedValue({
      path: OUTSIDE,
      hash: "abc",
      bytes: 12,
      changed: true,
      undoable: false,
    });
    const { verdict, getByRole } = await openCard({ ...APPLIED, path: OUTSIDE });

    fireEvent.click(getByRole("button", { name: "Undo" }));

    await waitFor(() =>
      expect(mocks.chatUndoProposal).toHaveBeenCalledWith("c1", 1, OUTSIDE, "abc", "def"),
    );
    await waitFor(() => expect(verdict()).toBe("README.md is back as it was."));
  });
});

describe("an offer for a file outside the notes folder", () => {
  it("waits for the file's tab, and is measured again when the tab opens and closes", async () => {
    const waiting: Partial<ChatProposal> = { path: OUTSIDE, needs_tab: true, hunks: [] };
    const { apply, container, getByRole } = await openCard(waiting);
    expect(container.textContent).toContain("Open the file to apply this.");
    expect(apply().disabled).toBe(true);
    fireEvent.click(apply());
    expect(mocks.chatApplyProposal).not.toHaveBeenCalled();

    fireEvent.click(getByRole("button", { name: "Open" }));
    await waitFor(() => expect(mocks.openFile).toHaveBeenCalledWith(OUTSIDE));

    const reads = mocks.chatOpen.mock.calls.length;
    mocks.chatOpen.mockResolvedValue(fileWith({ path: OUTSIDE }));
    bus.setTabs([
      { id: "b1", source_path: "/notes/Launch.md" },
      { id: "b9", source_path: OUTSIDE },
    ]);
    await waitFor(() => expect(apply().disabled).toBe(false));
    expect(mocks.chatOpen.mock.calls.length).toBe(reads + 1);
    expect(container.textContent).not.toContain("Open the file to apply this.");

    mocks.chatOpen.mockResolvedValue(fileWith(waiting));
    bus.setTabs([{ id: "b1", source_path: "/notes/Launch.md" }]);
    await waitFor(() => expect(apply().disabled).toBe(true));
    expect(container.textContent).toContain("Open the file to apply this.");
  });
});

describe("the listeners behind open offers", () => {
  it("read nothing once the pane closes, and listen again when it opens", async () => {
    await openCard();
    const reads = mocks.chatOpen.mock.calls.length;

    chatStore.closePane();
    await flush();
    saveTab("b1");
    emit("notes:changed", { path: "/notes/Launch.md", removed: false });
    emit("notes:indexed", { paths: ["/notes/Launch.md"] });
    emit("buffer:external", { bufferId: "b1", path: "/notes/Launch.md", change: "modified" });
    await flush();
    expect(mocks.chatOpen.mock.calls.length).toBe(reads);

    await chatStore.openPane();
    emit("notes:changed", { path: "/notes/Launch.md", removed: false });
    await waitFor(() => expect(mocks.chatOpen.mock.calls.length).toBe(reads + 1));
  });

  it("match a folder the watcher names through its resolved path", async () => {
    const { apply } = await openCard({ resolved_path: "/private/real/Launch.md" });
    const reads = mocks.chatOpen.mock.calls.length;

    mocks.chatOpen.mockResolvedValue(
      fileWith({ resolved_path: "/private/real/Launch.md", stale: true, hunks: [] }),
    );
    emit("notes:indexed", { paths: ["/private/real"] });

    await waitFor(() => expect(apply().disabled).toBe(true));
    expect(mocks.chatOpen.mock.calls.length).toBe(reads + 1);
  });
});
