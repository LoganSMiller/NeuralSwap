import { badge, byId, fileSize } from './bridge.ts';
import type { Candidate, FolderScan, RuntimeFile } from './types.ts';

/** Why a folder produced nothing, in terms a person can act on. */
const EMPTY_REASONS: Record<string, string> = {
  noExecutable: 'No programs found here, so this is probably not a game folder.',
  noGraphicsExecutable:
    'Programs were found, but none of them use a graphics API NeuralSwap can work with.',
  onlyHelpers: 'Only installers and helper programs were found here, not a game.',
  tooManyFiles:
    'This folder is too large to search. Pick the game’s own folder rather than a whole drive.',
  unreadable: 'That folder could not be read.'
};

/**
 * What each provenance verdict means.
 *
 * These describe evidence, not conclusions — the scanner cannot know what a
 * developer shipped, and saying so plainly beats a confident label that is
 * sometimes wrong.
 */
const PROVENANCE: Record<string, { label: string; why: string } | null> = {
  consistentWithSiblings: null,
  versionDiffersFromSiblings: {
    label: 'replaced',
    why: 'A different version from the other runtime files in the same folder. A game installs them as a matched set, so this one was probably swapped in.'
  },
  notBesideExecutable: {
    label: 'not where the game looks',
    why: 'This is not in the same folder as the game executable, so the loader will not pick it up. It was probably copied here by hand.'
  },
  ourInstall: {
    label: 'installed by NeuralSwap',
    why: 'NeuralSwap placed this file.'
  },
  unknown: null
};

function candidateRow(candidate: Candidate, recommended: boolean): HTMLTableRowElement {
  const row = document.createElement('tr');
  if (recommended) row.classList.add('recommended');

  const name = document.createElement('th');
  name.scope = 'row';
  name.textContent = candidate.rel;
  if (recommended) {
    name.append(' ', badge('recommended', 'The best match found in this folder.'));
  }
  if (candidate.likelyHelper) {
    name.append(
      ' ',
      badge(
        'looks like a launcher',
        'Ranked last because the name looks like a launcher or helper. It is still offered in case it is the game.',
        true
      )
    );
  }

  const api = document.createElement('td');
  api.textContent = candidate.api ? candidate.api.label : 'unknown';
  if (candidate.api?.fromMarker) {
    api.append(
      ' ',
      badge('inferred', 'Detected from a string in the binary, not its import table.', true)
    );
  }

  const bits = document.createElement('td');
  bits.textContent = `${candidate.bitness}-bit`;

  const size = document.createElement('td');
  size.className = 'numeric';
  size.textContent = fileSize(candidate.size);

  row.append(name, api, bits, size);
  return row;
}

function runtimeRow(file: RuntimeFile): HTMLLIElement {
  const row = document.createElement('li');

  const name = document.createElement('span');
  name.className = 'runtime-name';
  name.textContent = file.rel;
  row.append(name);

  if (file.version) {
    const version = document.createElement('span');
    version.className = 'runtime-version';
    version.textContent = `v${file.version}`;
    row.append(' ', version);
  }

  const note = PROVENANCE[file.provenance];
  if (note) row.append(' ', badge(note.label, note.why, file.provenance !== 'ourInstall'));
  return row;
}

export function renderScan(scan: FolderScan): void {
  const path = byId('scan-path');
  path.textContent = scan.dir;
  path.hidden = false;

  const result = byId('scan-result');
  const empty = byId('scan-empty');
  const body = byId<HTMLTableElement>('candidates').tBodies[0];
  if (!body) return;
  body.replaceChildren();

  if (scan.candidates.length === 0) {
    result.hidden = true;
    empty.textContent =
      (scan.reason ? EMPTY_REASONS[scan.reason] : null) ??
      'Nothing installable was found here.';
    empty.hidden = false;
    return;
  }

  empty.hidden = true;
  for (const [index, candidate] of scan.candidates.entries()) {
    body.append(candidateRow(candidate, index === scan.chosen));
  }

  const runtimes = byId('runtimes');
  const list = byId<HTMLUListElement>('runtime-list');
  list.replaceChildren();
  if (scan.runtimeFiles.length === 0) {
    runtimes.hidden = true;
  } else {
    // Anything unusual first: that is what somebody needs to see.
    const ordered = [...scan.runtimeFiles].sort((a, b) => {
      const rank = (file: RuntimeFile): number => (PROVENANCE[file.provenance] ? 0 : 1);
      return rank(a) - rank(b) || a.rel.localeCompare(b.rel);
    });
    for (const file of ordered) list.append(runtimeRow(file));
    runtimes.hidden = false;
  }

  const flagged = scan.runtimeFiles.filter((file) => PROVENANCE[file.provenance]).length;
  const notes: string[] = [];
  if (flagged > 0) {
    const total = scan.runtimeFiles.length;
    notes.push(
      `${flagged} of ${total} runtime file${total === 1 ? '' : 's'} look${flagged === 1 ? 's' : ''} to have been added or replaced.`
    );
  }
  if (scan.excluded.length > 0) {
    const count = scan.excluded.length;
    notes.push(`${count} installer or helper program${count === 1 ? '' : 's'} skipped.`);
  }
  notes.push(
    `Searched ${scan.stats.entriesExamined} entries in ${scan.stats.walkMs} ms; read ${scan.stats.binariesParsed} binaries in ${scan.stats.parseMs} ms.`
  );
  byId('scan-extra').textContent = notes.join(' ');
  result.hidden = false;
}

export function clearScan(): void {
  byId('scan-result').hidden = true;
  byId('scan-empty').hidden = true;
  byId('runtimes').hidden = true;
  byId('scan-path').hidden = true;
}
