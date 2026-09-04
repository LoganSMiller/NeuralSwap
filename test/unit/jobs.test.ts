import assert from 'node:assert/strict';
import { test } from 'node:test';
import { KeyedLock, pooledMap } from '../../src/main/jobs/queue.ts';
import { Sweeper } from '../../src/main/jobs/sweep.ts';
import { AppError } from '../../src/shared/errors.ts';

const tick = (ms = 0): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

test('KeyedLock serialises work on the same key', async () => {
  const lock = new KeyedLock();
  const order: string[] = [];

  const job = (name: string, delay: number) => async (): Promise<void> => {
    order.push(`${name}:start`);
    await tick(delay);
    order.push(`${name}:end`);
  };

  await Promise.all([
    lock.run('D:\\Games\\Skyrim', job('a', 20)),
    lock.run('D:\\Games\\Skyrim', job('b', 1))
  ]);

  // b must not begin until a has finished, or both would be writing the same
  // game folder at once.
  assert.deepEqual(order, ['a:start', 'a:end', 'b:start', 'b:end']);
});

test('KeyedLock treats paths case-insensitively', async () => {
  const lock = new KeyedLock();
  const order: string[] = [];
  await Promise.all([
    lock.run('D:\\Games\\Skyrim', async () => {
      order.push('a:start');
      await tick(15);
      order.push('a:end');
    }),
    // Windows paths differing only in case are the same folder.
    lock.run('d:\\games\\skyrim', async () => {
      order.push('b:start');
    })
  ]);
  assert.deepEqual(order, ['a:start', 'a:end', 'b:start']);
});

test('KeyedLock runs different keys concurrently', async () => {
  const lock = new KeyedLock();
  const order: string[] = [];
  await Promise.all([
    lock.run('game-a', async () => {
      order.push('a:start');
      await tick(20);
      order.push('a:end');
    }),
    lock.run('game-b', async () => {
      order.push('b:start');
      await tick(1);
      order.push('b:end');
    })
  ]);
  assert.deepEqual(order, ['a:start', 'b:start', 'b:end', 'a:end']);
});

test('tryRun refuses immediately rather than queueing a second install', async () => {
  const lock = new KeyedLock();
  let ran = 0;
  const first = lock.tryRun('game', async () => {
    ran += 1;
    await tick(20);
  });

  await assert.rejects(
    () => lock.tryRun('game', async () => { ran += 1; }),
    (cause: unknown) => cause instanceof AppError && cause.code === 'jobBusy'
  );

  await first;
  // The refused job must not fire later - a double-click on Install should not
  // silently repeat the whole operation minutes afterwards.
  assert.equal(ran, 1);
});

test('the lock is released after a failure', async () => {
  const lock = new KeyedLock();
  await assert.rejects(() => lock.tryRun('game', async () => { throw new Error('boom'); }));
  assert.equal(lock.isBusy('game'), false);
  await lock.tryRun('game', async () => {});
});

test('isBusy reports the specific key, not the whole app', async () => {
  const lock = new KeyedLock();
  const running = lock.run('game-a', () => tick(20));
  assert.equal(lock.isBusy('game-a'), true);
  assert.equal(lock.isBusy('game-b'), false);
  await running;
  assert.equal(lock.isBusy('game-a'), false);
});

test('pooledMap respects its concurrency ceiling', async () => {
  let inFlight = 0;
  let peak = 0;
  const items = Array.from({ length: 40 }, (_, i) => i);

  const results = await pooledMap(items, 5, async (item) => {
    inFlight += 1;
    peak = Math.max(peak, inFlight);
    await tick(2);
    inFlight -= 1;
    return item * 2;
  });

  assert.equal(peak, 5);
  assert.deepEqual(results.slice(0, 4), [0, 2, 4, 6]);
  assert.equal(results.length, 40);
});

test('pooledMap survives a worker that throws', async () => {
  const results = await pooledMap([1, 2, 3, 4], 2, async (item) => {
    if (item % 2 === 0) throw new Error('unreadable folder');
    return item;
  });
  // One unreadable folder must not abandon the rest of the library.
  assert.deepEqual(results, [1, null, 3, null]);
});

test('pooledMap stops promptly when aborted', async () => {
  const controller = new AbortController();
  let started = 0;
  const run = pooledMap(Array.from({ length: 200 }, (_, i) => i), 2, async (item) => {
    started += 1;
    await tick(1);
    return item;
  }, { signal: controller.signal });

  await tick(10);
  controller.abort();
  await run;
  assert.ok(started < 200, `expected an early stop, started ${started}`);
});

test('a new sweep supersedes the one already running', async () => {
  const sweeper = new Sweeper();
  const firstResults: number[] = [];
  const secondResults: number[] = [];

  const items = Array.from({ length: 100 }, (_, i) => i);
  const first = sweeper.run(items, async (item) => {
    await tick(2);
    return item;
  }, { concurrency: 2, onResult: (value) => firstResults.push(value) });

  await tick(15);
  const second = sweeper.run(items, async (item) => {
    await tick(1);
    return item * 10;
  }, { concurrency: 4, onResult: (value) => secondResults.push(value) });

  const firstOutcome = await first;
  const secondOutcome = await second;

  // This is the re-entrant `load()` case: the older pass must report that it
  // was superseded, and must stop feeding results into shared state.
  assert.equal(firstOutcome.completed, false);
  assert.equal(secondOutcome.completed, true);
  assert.ok(firstResults.length < 100, `first sweep should have stopped early, got ${firstResults.length}`);
  assert.equal(secondResults.length, 100);

  const countAfter = firstResults.length;
  await tick(30);
  assert.equal(firstResults.length, countAfter, 'superseded sweep kept emitting results');
});

test('sweep progress counts to the total exactly once', async () => {
  const sweeper = new Sweeper();
  const progress: number[] = [];
  const outcome = await sweeper.run([1, 2, 3, 4, 5], async (item) => item, {
    concurrency: 2,
    onProgress: (p) => progress.push(p.done)
  });

  assert.equal(outcome.completed, true);
  assert.equal(progress[0], 0);
  assert.equal(progress.at(-1), 5);
  // Monotonic, no double-counting.
  assert.deepEqual(progress, [...progress].sort((a, b) => a - b));
  assert.equal(new Set(progress).size, progress.length);
});

test('an explicitly cancelled sweep reports incomplete', async () => {
  const sweeper = new Sweeper();
  const run = sweeper.run(Array.from({ length: 100 }, (_, i) => i), async (item) => {
    await tick(2);
    return item;
  }, { concurrency: 2 });

  await tick(10);
  sweeper.cancel();
  const outcome = await run;
  assert.equal(outcome.completed, false);
  assert.equal(sweeper.running, false);
});
