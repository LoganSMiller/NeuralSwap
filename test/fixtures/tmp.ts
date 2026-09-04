import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { after } from 'node:test';

/**
 * A scratch directory that removes itself when the test file finishes, so a
 * failing assertion never leaves the next run to trip over stale state.
 */
export function scratch(label: string): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `neuralswap-${label}-`));
  after(() => {
    fs.rmSync(dir, { recursive: true, force: true });
  });
  return dir;
}
