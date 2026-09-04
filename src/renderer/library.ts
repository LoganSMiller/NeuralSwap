import type { FolderScan } from './types.ts';
import { call } from './bridge.ts';

/**
 * The library view: what is installed, and what each folder turns out to
 * contain once scanned.
 *
 * Games are listed from storefront records immediately - that takes
 * milliseconds - and each one is scanned lazily when opened, because reading
 * executable headers for a whole library up front is work nobody asked for.
 */

export interface Game {
  name: string;
  dir: string;
  source: 'steam' | 'epic' | 'xbox' | 'manual';
  appId: string | null;
}

const SOURCE_LABELS: Record<Game['source'], string> = {
  steam: 'Steam',
  epic: 'Epic Games',
  xbox: 'Xbox',
  manual: 'Added by hand'
};

/** Two initials, for a game with no artwork. */
export function initials(name: string): string {
  const words = name
    .replace(/[^\p{L}\p{N} ]/gu, '')
    .split(/\s+/)
    .filter(Boolean);
  const letters = words
    .slice(0, 2)
    .map((word) => [...word][0] ?? '')
    .join('');
  return letters.toUpperCase() || '?';
}

export interface GameCardHandlers {
  onOpen: (game: Game) => void;
}

export function gameCard(game: Game, handlers: GameCardHandlers): HTMLElement {
  const card = document.createElement('button');
  card.type = 'button';
  card.className = 'game-card';
  // The whole card is the control, so it needs one accessible name rather
  // than three unrelated text nodes read in sequence.
  card.setAttribute(
    'aria-label',
    `${game.name}, from ${SOURCE_LABELS[game.source]}. Open to scan.`
  );

  const mark = document.createElement('span');
  mark.className = 'game-initials';
  mark.setAttribute('aria-hidden', 'true');
  mark.textContent = initials(game.name);

  const text = document.createElement('span');
  text.className = 'game-text';

  const title = document.createElement('span');
  title.className = 'game-name';
  title.textContent = game.name;

  const source = document.createElement('span');
  source.className = 'game-source';
  source.textContent = SOURCE_LABELS[game.source];

  text.append(title, source);
  card.append(mark, text);
  card.addEventListener('click', () => handlers.onOpen(game));
  return card;
}

/** Group games under their storefront, or return one unlabelled group. */
export function groupGames(
  games: Game[],
  byStore: boolean
): { label: string | null; games: Game[] }[] {
  if (!byStore) return [{ label: null, games }];

  const order: Game['source'][] = ['steam', 'epic', 'xbox', 'manual'];
  return order
    .map((source) => ({
      label: SOURCE_LABELS[source],
      games: games.filter((game) => game.source === source)
    }))
    .filter((group) => group.games.length > 0);
}

export async function loadGames(): Promise<Game[]> {
  return call<Game[]>('library_list');
}

export async function scanGame(game: Game): Promise<FolderScan> {
  return call<FolderScan>('scan_folder', { dir: game.dir });
}
