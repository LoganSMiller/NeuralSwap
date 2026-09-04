/**
 * Builds the frontend into `dist/renderer`, which is what `tauri.conf.json`
 * points `frontendDist` at.
 *
 * There is no dev server: the UI is a static page, so `tauri dev` and
 * `tauri build` both just run this first. One less moving part than a bundler
 * in watch mode proxied through a dev origin, and it means the page the
 * developer sees is loaded exactly the way the shipped one is.
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import esbuild from 'esbuild';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const out = path.join(root, 'dist', 'renderer');
const watch = process.argv.includes('--watch');

fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(out, { recursive: true });

for (const asset of ['index.html', 'app.css']) {
  fs.copyFileSync(path.join(root, 'src/renderer', asset), path.join(out, asset));
}

const options = {
  entryPoints: [path.join(root, 'src/renderer/app.ts')],
  outfile: path.join(out, 'app.js'),
  bundle: true,
  format: 'iife',
  platform: 'browser',
  target: 'chrome120',
  minify: !watch,
  sourcemap: true,
  logLevel: 'info'
};

if (watch) {
  const context = await esbuild.context(options);
  await context.watch();
  console.log('watching the frontend');
} else {
  await esbuild.build(options);
  const size = (fs.statSync(options.outfile).size / 1024).toFixed(1);
  console.log(`frontend built: app.js ${size} kB`);
}
