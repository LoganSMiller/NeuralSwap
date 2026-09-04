import { AppError } from '../../shared/errors.ts';

/** Wait for a promise to settle, caring only that it is over. */
const heldUntilSettled = (promise: Promise<unknown>): Promise<void> =>
  promise.then(
    () => undefined,
    () => undefined
  );

/**
 * Serialises work by key, and refuses rather than queues when asked to.
 *
 * Upstream guarded installs with a single module-level `mutationBusy` boolean.
 * That is correct for "one install at a time" but it conflates two different
 * needs: an install must not overlap *itself* for one game, and it must not
 * overlap a *different* game either, because both touch shared component
 * caches. It also gives no way to ask whether a specific game is busy, which
 * is what the UI needs to disable one card rather than the whole window.
 */
export class KeyedLock {
  /** Key -> promise that settles when the current holder finishes. */
  readonly #held = new Map<string, Promise<unknown>>();

  private static norm(key: string): string {
    return key.toLowerCase();
  }

  isBusy(key: string): boolean {
    return this.#held.has(KeyedLock.norm(key));
  }

  get busyKeys(): string[] {
    return [...this.#held.keys()];
  }

  /** Queue behind any current holder of `key`. */
  run<T>(key: string, work: () => Promise<T>): Promise<T> {
    const id = KeyedLock.norm(key);
    const previous = this.#held.get(id) ?? Promise.resolve();

    // The slot stays occupied until this gate opens, which happens in the
    // `finally` below - so a waiter cannot start before the holder is done.
    let open!: () => void;
    const gate = new Promise<void>((resolve) => {
      open = resolve;
    });
    // Swallow the predecessor's rejection here: a failed job must release the
    // lock, not poison every job queued behind it.
    const heldUntil = previous.then(
      () => gate,
      () => gate
    );
    this.#held.set(id, heldUntil);

    return (async () => {
      await heldUntilSettled(previous);
      try {
        return await work();
      } finally {
        // Clearing the slot inside `finally` means it is already free by the
        // time the caller's `await` resumes, so `isBusy` cannot lie to the UI.
        // Only clear it if nobody queued behind us, or a later waiter would be
        // reported as idle while it is still running.
        if (this.#held.get(id) === heldUntil) this.#held.delete(id);
        open();
      }
    })();
  }

  /**
   * Run only if the key is free. Installing is destructive and slow, so a
   * second click should be told "busy" immediately rather than silently
   * queued to fire again several minutes later.
   */
  tryRun<T>(key: string, work: () => Promise<T>): Promise<T> {
    if (this.isBusy(key)) {
      return Promise.reject(new AppError('jobBusy', 'another job is running', { key }));
    }
    return this.run(key, work);
  }
}

/**
 * Map over items with bounded concurrency, cooperatively cancellable.
 *
 * Scanning a library is IO-bound on a lot of small header reads, so doing it
 * one game at a time over sequential IPC round-trips - as upstream does - is
 * dominated by latency rather than work. A small pool keeps several folders in
 * flight without thrashing the disk queue.
 */
export async function pooledMap<T, R>(
  items: readonly T[],
  concurrency: number,
  worker: (item: T, index: number, signal: AbortSignal) => Promise<R>,
  options: { signal?: AbortSignal; onSettled?: (result: R | null, index: number) => void } = {}
): Promise<(R | null)[]> {
  const width = Math.max(1, Math.min(concurrency, items.length));
  const out: (R | null)[] = Array.from({ length: items.length }, () => null);
  const signal = options.signal ?? new AbortController().signal;
  let cursor = 0;

  const drain = async (): Promise<void> => {
    while (!signal.aborted) {
      const index = cursor++;
      if (index >= items.length) return;
      let value: R | null = null;
      try {
        value = await worker(items[index] as T, index, signal);
      } catch {
        // One unreadable folder must not abandon the other ninety-nine. The
        // worker is responsible for turning its own failure into a result.
        value = null;
      }
      if (signal.aborted) return;
      out[index] = value;
      options.onSettled?.(value, index);
    }
  };

  await Promise.all(Array.from({ length: width }, drain));
  return out;
}
