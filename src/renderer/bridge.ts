import { invoke } from '@tauri-apps/api/core';

/** A command failure arrives as the serialised core error, not as a string. */
interface CoreError {
  code: string;
  message: string;
}

function isCoreError(value: unknown): value is CoreError {
  return (
    typeof value === 'object' &&
    value !== null &&
    typeof (value as { code?: unknown }).code === 'string'
  );
}

/**
 * Every call funnels through here so a rejection becomes an `Error` carrying
 * the stable code, rather than whatever shape the boundary handed back.
 */
export async function call<T>(
  command: string,
  args?: Record<string, unknown>
): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (raw) {
    if (isCoreError(raw)) {
      throw Object.assign(new Error(raw.message || raw.code), { code: raw.code });
    }
    throw Object.assign(new Error(String(raw)), { code: 'internal' });
  }
}

export function codeOf(error: unknown): string | undefined {
  return (error as { code?: string }).code;
}

export const byId = <T extends HTMLElement>(id: string): T => {
  const found = document.getElementById(id);
  if (!found) throw new Error(`missing element: ${id}`);
  return found as T;
};

/**
 * A small labelled pill.
 *
 * Every view builds these rather than assembling markup: the text is often a
 * name taken off the filesystem, and `textContent` cannot be talked into
 * being markup.
 */
export function badge(text: string, title: string, muted = false): HTMLSpanElement {
  const node = document.createElement('span');
  node.className = muted ? 'badge muted-badge' : 'badge';
  node.textContent = text;
  node.title = title;
  return node;
}

const MB = 1024 * 1024;

export function fileSize(bytes: number): string {
  if (bytes >= MB) return `${(bytes / MB).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} kB`;
  return `${bytes} B`;
}
