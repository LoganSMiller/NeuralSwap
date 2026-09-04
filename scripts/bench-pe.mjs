/**
 * Measures the PE reader against the upstream approach it replaces.
 *
 * Two metrics, because they answer different questions:
 *
 *   bytes read - deterministic, and the honest measure of how much IO a scan
 *                strategy costs. Wall time on a warm file measures the OS
 *                page cache more than it measures the parser.
 *   wall time  - reported as the best of several warmed runs, so a cold cache
 *                or a background flush does not masquerade as a result.
 *
 * Run with the upstream checkout available:
 *   node scripts/bench-pe.mjs --upstream <path-to-DLSS5-Swapper>
 */
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createRequire } from 'node:module';
import { performance } from 'node:perf_hooks';
import { PeFile } from '../src/core/pe/reader.ts';
import { PeCache } from '../src/core/pe/summary.ts';

const MARKERS = [
  'D3D12CreateDevice',
  'D3D12SDKPath',
  'D3D12SDKVersion',
  'D3D11CreateDevice',
  'D3D10CreateDevice',
  'Direct3DCreate9',
  'Direct3DCreate8',
  'CreateDXGIFactory',
  'vkCreateInstance',
  'wglCreateContext'
];

const REQUEST = {
  markers: MARKERS,
  versionStrings: ['ReShade', 'dgVoodoo'],
  probes: { addonLoader: 'Searching for add-ons' },
  rules: 1
};

const RUNS = 3;

function argValue(name) {
  const at = process.argv.indexOf(name);
  return at !== -1 ? process.argv[at + 1] : null;
}

function corpus(limit) {
  const files = [];
  const system = path.join(process.env.SystemRoot ?? 'C:\\Windows', 'System32');
  try {
    for (const name of fs.readdirSync(system).filter((n) => /\.(dll|exe)$/i.test(n))) {
      const file = path.join(system, name);
      try {
        if (fs.statSync(file).isFile()) files.push(file);
      } catch {
        /* locked or protected; skip */
      }
      if (files.length >= limit) break;
    }
  } catch {
    /* no System32 on this platform */
  }
  return files;
}

/** Pull every byte through the page cache so timings measure work, not IO. */
function warm(files) {
  let total = 0;
  for (const file of files) {
    try {
      total += fs.readFileSync(file).length;
    } catch {
      /* unreadable; the parsers will skip it too */
    }
  }
  return total;
}

function best(runs, work) {
  let fastest = Infinity;
  let detail = null;
  for (let i = 0; i < runs; i += 1) {
    const started = performance.now();
    detail = work();
    fastest = Math.min(fastest, performance.now() - started);
  }
  return { ms: fastest, detail };
}

const mb = (bytes) => `${(bytes / 1048576).toFixed(1)} MB`;
const row = (label, ms, detail) =>
  console.log(`  ${label.padEnd(32)} ${ms.toFixed(0).padStart(6)} ms   ${detail}`);

const upstreamRoot = argValue('--upstream');
let upstream = null;
if (upstreamRoot) {
  const entry = path.join(upstreamRoot, 'src', 'core', 'pe.js');
  if (fs.existsSync(entry)) upstream = createRequire(import.meta.url)(entry);
  else console.log(`! no pe.js under ${upstreamRoot}; skipping the comparison\n`);
}

const files = corpus(300);
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'neuralswap-bench-'));

try {
  console.log(`corpus: ${files.length} real binaries, ${mb(warm(files))} warmed into cache\n`);

  // ---- Identify every binary the way the scanner needs to ----
  console.log('full identification pass (bitness + imports + version + markers)');

  let ourBytes = 0;
  const ours = best(RUNS, () => {
    ourBytes = 0;
    let parsed = 0;
    for (const file of files) {
      const read = PeFile.with(
        file,
        (pe) => {
          pe.imports();
          pe.fileVersion();
          pe.versionMentions('ReShade');
          pe.findMarkers(MARKERS);
          return pe.bytesRead;
        },
        0
      );
      ourBytes += read;
      if (read > 0) parsed += 1;
    }
    return `${parsed} parsed, ${mb(ourBytes)} read`;
  });
  row('ours', ours.ms, ours.detail);

  let upstreamRun = null;
  if (upstream) {
    // Upstream's findMarkers reads the whole file when a marker is absent, and
    // each of the five calls re-reads the headers. File size is the floor.
    const upstreamBytes = files.reduce((sum, file) => {
      try {
        return sum + fs.statSync(file).size;
      } catch {
        return sum;
      }
    }, 0);
    upstreamRun = best(RUNS, () => {
      let parsed = 0;
      for (const file of files) {
        if (upstream.getBitness(file)) parsed += 1;
        upstream.getImports(file);
        upstream.getFileVersion(file);
        upstream.versionMentions(file, 'ReShade');
        upstream.findMarkers(file, MARKERS);
      }
      return `${parsed} parsed, >= ${mb(upstreamBytes)} read`;
    });
    row('upstream', upstreamRun.ms, upstreamRun.detail);
    console.log(`\n  time: ${(upstreamRun.ms / ours.ms).toFixed(2)}x`);
  }

  // ---- The rescan, which is what users actually pay for on every launch ----
  console.log('\nrescan of the same library');
  const cache = new PeCache();
  const cold = best(1, () => {
    for (const file of files) cache.summarize(file, REQUEST);
    return `${cache.stats.misses} parsed`;
  });
  const hot = best(RUNS, () => {
    for (const file of files) cache.summarize(file, REQUEST);
    return `${cache.size} entries, 0 re-read`;
  });
  row('first scan', cold.ms, cold.detail);
  row('rescan, nothing changed', hot.ms, hot.detail);
  console.log(`\n  time: ${(cold.ms / Math.max(hot.ms, 0.001)).toFixed(0)}x`);

  // ---- An executable with a large appended overlay ----
  console.log('\nexecutable with a large appended overlay');
  const big = path.join(scratch, 'overlay.exe');
  const base = fs.readFileSync(
    path.join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'kernel32.dll')
  );
  fs.writeFileSync(big, Buffer.concat([base, Buffer.alloc(220 * 1024 * 1024, 0x20)]));
  console.log(`  image: ${mb(fs.statSync(big).size)}, of which ${mb(base.length)} is mapped sections`);
  warm([big]);

  const oursBig = best(RUNS, () => {
    const read = PeFile.with(
      big,
      (pe) => {
        pe.findMarkers(MARKERS);
        return pe.bytesRead;
      },
      0
    );
    return `${mb(read)} read`;
  });
  row('ours, mapped sections only', oursBig.ms, oursBig.detail);

  if (upstream) {
    const upstreamBig = best(RUNS, () => {
      upstream.findMarkers(big, MARKERS);
      return `${mb(fs.statSync(big).size)} read`;
    });
    row('upstream, whole file', upstreamBig.ms, upstreamBig.detail);
    console.log(`\n  time: ${(upstreamBig.ms / Math.max(oursBig.ms, 0.001)).toFixed(1)}x`);
  }
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}
