import { pooledMap } from './queue.ts';

export interface SweepProgress {
  done: number;
  total: number;
}

export interface SweepOutcome<R> {
  /** False when a newer sweep superseded this one. */
  completed: boolean;
  results: (R | null)[];
}

/**
 * A long-running background pass over the library - scan every folder, fetch
 * every missing cover - of which only the newest is ever wanted.
 *
 * The bug this exists to prevent: upstream calls `load()` from eight places,
 * mostly without awaiting it, and `load()` kicks off `scanAll()` which kicks
 * off `fetchArt()`. Add a folder while the first sweep is still running and
 * two loops now interleave, both writing the same settings keys, both driving
 * the same progress bar, and both asking Steam for the same artwork.
 *
 * Starting a sweep therefore cancels the one before it. Late results from a
 * superseded generation are dropped instead of overwriting fresher ones.
 */
export class Sweeper {
  #controller: AbortController | null = null;
  #generation = 0;

  get running(): boolean {
    return this.#controller !== null;
  }

  cancel(): void {
    this.#controller?.abort();
    this.#controller = null;
  }

  async run<T, R>(
    items: readonly T[],
    worker: (item: T, index: number, signal: AbortSignal) => Promise<R>,
    options: {
      concurrency?: number;
      onResult?: (result: R, item: T, index: number) => void;
      onProgress?: (progress: SweepProgress) => void;
    } = {}
  ): Promise<SweepOutcome<R>> {
    // Supersede whatever was in flight before touching any shared state.
    this.cancel();
    const controller = new AbortController();
    this.#controller = controller;
    const generation = ++this.#generation;
    const mine = (): boolean => this.#generation === generation && !controller.signal.aborted;

    let done = 0;
    options.onProgress?.({ done: 0, total: items.length });

    const results = await pooledMap(items, options.concurrency ?? 4, worker, {
      signal: controller.signal,
      onSettled: (value, index) => {
        if (!mine()) return;
        done += 1;
        if (value !== null) options.onResult?.(value, items[index] as T, index);
        options.onProgress?.({ done, total: items.length });
      }
    });

    const completed = mine();
    if (this.#controller === controller) this.#controller = null;
    return { completed, results };
  }
}
