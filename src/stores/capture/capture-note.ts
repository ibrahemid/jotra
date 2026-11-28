import { createRoot, createSignal } from "solid-js";
import * as api from "../../services/tauri";
import { describeSaveFailure } from "../../lib/save-error";
import { logFailure } from "../../lib/log";

/** The calls the note makes, as a seam for tests. */
export interface CaptureCalls {
  append: (text: string) => Promise<api.CaptureOutcome>;
  target: () => Promise<string>;
  close: (returnFocus: boolean) => Promise<void>;
  draft: () => Promise<string>;
  keep: (text: string) => Promise<void>;
}

const SERVICE_CALLS: CaptureCalls = {
  append: (text) => api.appendCapture(text),
  target: () => api.captureTarget(),
  close: (returnFocus) => api.closeCapture(returnFocus),
  draft: () => api.captureDraft(),
  keep: (text) => api.keepCaptureDraft(text),
};

const CODED = /^ERR_[A-Z_]+/;

/** What the note says when its entry was not written. A coded failure reads
 * as the editor's save would put it; a sentence from the backend is shown as
 * it is. */
export function captureFailureMessage(error: unknown, name: string): string {
  const raw = typeof error === "string" ? error : error instanceof Error ? error.message : "";
  const file = name || "today's file";
  if (raw !== "" && !CODED.test(raw)) return raw;
  const reason = describeSaveFailure(error);
  if (reason.code === "ERR_FILE_CHANGED_ON_DISK") {
    return `${file} kept changing while this was being added. Press Escape to try again.`;
  }
  return `Could not add this to ${file}: ${reason.message}`;
}

// Singleton state — the capture window's own realm (ADR-055). One note per
// window, and the main window's stores never load here.

export function createCaptureNoteStore(calls: CaptureCalls = SERVICE_CALLS) {
  const [text, setTextSignal] = createSignal("");
  const [target, setTarget] = createSignal("");
  const [problem, setProblem] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  let edited = false;

  /** Changes the text and hands it to the app, which keeps it across a quit. */
  function setText(value: string): void {
    edited = true;
    setTextSignal(value);
    calls.keep(value).catch(() => logFailure("the capture note's text could not be kept"));
  }

  /** Puts back the text the note held when the app last quit, unless
   * something was typed since. */
  async function restoreDraft(): Promise<void> {
    let draft = "";
    try {
      draft = await calls.draft();
    } catch {
      logFailure("the capture note's last text could not be read");
      return;
    }
    if (!edited && draft !== "") setTextSignal(draft);
  }

  async function close(returnFocus: boolean): Promise<void> {
    try {
      await calls.close(returnFocus);
    } catch {
      logFailure("the capture note could not be hidden");
    }
  }

  /** Reads the name of the file the next entry goes to. */
  async function refreshTarget(): Promise<void> {
    try {
      setTarget(await calls.target());
    } catch {
      setTarget("");
    }
  }

  /** Escape: adds what was typed to today's file, then hides the note and
   * hands the keyboard back. A failure keeps the note, the text and the
   * reason. */
  async function submit(): Promise<void> {
    if (busy()) return;
    if (text().trim() === "") {
      setProblem(null);
      await close(true);
      return;
    }
    setBusy(true);
    try {
      const outcome = await calls.append(text());
      setText("");
      setProblem(null);
      setTarget(outcome.name);
      await close(true);
    } catch (error) {
      setProblem(captureFailureMessage(error, target()));
    } finally {
      setBusy(false);
    }
  }

  /** The keyboard went somewhere else: the note hides and keeps its text. */
  async function dismiss(): Promise<void> {
    if (busy()) return;
    await close(false);
  }

  return { text, setText, target, problem, busy, refreshTarget, restoreDraft, submit, dismiss };
}

export const captureNoteStore = createRoot(() => createCaptureNoteStore());
