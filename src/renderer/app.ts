import { byId, call, codeOf } from './bridge.ts';
import { gameCard, groupGames, loadGames, scanGame, type Game } from './library.ts';
import { clearScan, renderScan } from './scan-view.ts';
import type { BootInfo, CacheInfo, FolderScan, Health, LoadStatus, Settings } from './types.ts';

/**
 * Views build DOM nodes and set textContent. Nothing here assembles markup
 * from strings.
 *
 * This is not stylistic. The upstream project has sixteen `innerHTML` template
 * sites where an `esc()` call has to be remembered at every interpolation, and
 * the values interpolated are executable and folder names taken off the
 * filesystem. One missed call turns a folder called `<img onerror=...>` into
 * script running inside the window that holds the bridge to the filesystem.
 */

// ---------------------------------------------------------------- health

const HEALTH_MESSAGES: Record<LoadStatus, { title: string; detail: string } | null> = {
  fresh: null,
  loaded: null,
  migrated: {
    title: 'Settings updated',
    detail:
      'Your settings were written by an earlier version and have been brought forward.'
  },
  recoveredFromBackup: {
    title: 'Settings recovered',
    detail:
      'The settings file could not be read, so the previous good copy was used instead. Recent changes may be missing.'
  },
  quarantined: {
    title: 'Settings could not be read',
    detail:
      'The settings file was unreadable and has been set aside rather than deleted, and defaults are in use. The old file is kept at:'
  }
};

function showHealth(health: Health): void {
  const message = HEALTH_MESSAGES[health.status];
  if (!message && !health.writeError) return;

  const section = byId('health');
  if (health.writeError) {
    byId('health-title').textContent = 'Settings are not being saved';
    byId('health-detail').textContent =
      'Changes are held in memory only, because the settings file could not be written. Check that the disk is not full and that the folder is writable.';
    section.classList.add('warn');
  } else if (message) {
    byId('health-title').textContent = message.title;
    byId('health-detail').textContent = health.quarantinedTo
      ? `${message.detail} ${health.quarantinedTo}`
      : message.detail;
    if (health.status !== 'migrated') section.classList.add('warn');
  }
  section.hidden = false;
}

// ---------------------------------------------------------------- library

let games: Game[] = [];
let groupByStore = true;
let scanning = false;

function renderLibrary(): void {
  const container = byId('library');
  container.replaceChildren();

  if (games.length === 0) {
    const empty = document.createElement('p');
    empty.className = 'muted';
    empty.textContent =
      'No installed games found. Steam, Epic and Xbox libraries are read automatically — or add a folder by hand.';
    container.append(empty);
    return;
  }

  for (const group of groupGames(games, groupByStore)) {
    const section = document.createElement('section');
    section.className = 'game-group';
    if (group.label) {
      const heading = document.createElement('h3');
      heading.textContent = `${group.label} · ${group.games.length}`;
      section.append(heading);
    }
    const grid = document.createElement('div');
    grid.className = 'game-grid';
    for (const game of group.games) {
      grid.append(gameCard(game, { onOpen: (picked) => void openGame(picked) }));
    }
    section.append(grid);
    container.append(section);
  }
}

async function refreshLibrary(): Promise<void> {
  const status = byId('library-status');
  status.textContent = 'Reading storefront records…';
  try {
    games = await loadGames();
    status.textContent = `${games.length} game${games.length === 1 ? '' : 's'} found.`;
    renderLibrary();
  } catch (error) {
    status.textContent =
      error instanceof Error ? error.message : 'Could not read the library.';
  }
}

/** Scan one game and show the result. */
async function openGame(game: Game): Promise<void> {
  if (scanning) return;
  scanning = true;
  clearScan();
  byId('scan-title').textContent = game.name;
  byId('scan-panel').hidden = false;
  byId('scan-hint').textContent = 'Reading executable headers…';
  byId('scan-panel').scrollIntoView({ behavior: 'smooth', block: 'nearest' });

  try {
    const scan = await scanGame(game);
    renderScan(scan);
    byId('scan-hint').textContent = 'Nothing was written.';
  } catch (error) {
    const empty = byId('scan-empty');
    empty.textContent = error instanceof Error ? error.message : 'The scan failed.';
    empty.hidden = false;
    byId('scan-hint').textContent = '';
  } finally {
    scanning = false;
    await refreshCacheFact();
  }
}

// ---------------------------------------------------------------- settings

let saveTimer: number | undefined;
function reportSaved(text: string, bad = false): void {
  const status = byId('save-state');
  status.textContent = text;
  status.classList.toggle('warn-text', bad);
  window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    status.textContent = '';
  }, bad ? 8000 : 1800);
}

async function persist(work: () => Promise<unknown>): Promise<void> {
  try {
    await work();
    reportSaved('Saved');
  } catch (error) {
    reportSaved(
      codeOf(error) === 'stateUnwritable'
        ? 'Could not save — see the notice above'
        : 'Could not save',
      true
    );
    showHealth(await call<Health>('settings_health'));
  }
}

function fact(list: HTMLElement, label: string, value: string, id?: string): void {
  const term = document.createElement('dt');
  term.textContent = label;
  const description = document.createElement('dd');
  description.textContent = value;
  if (id) description.id = id;
  list.append(term, description);
}

async function refreshCacheFact(): Promise<void> {
  try {
    const info = await call<CacheInfo>('scan_cache_info');
    const slot = document.getElementById('cache-count');
    if (slot) slot.textContent = String(info.entries);
  } catch {
    // A cache figure is a nicety; failing to read it is not worth reporting.
  }
}

// ---------------------------------------------------------------- start

async function main(): Promise<void> {
  const boot = await call<BootInfo>('app_boot');
  byId('version').textContent = `v${boot.version}`;
  document.documentElement.dataset['theme'] = boot.theme;
  showHealth(boot.settingsHealth);
  groupByStore = boot.groupGamesByStore;

  const settings = await call<Settings>('settings_read');
  const facts = byId('facts');
  fact(facts, 'Settings schema', String(settings.schema));
  fact(facts, 'Folders added by hand', String(settings.manual.length));
  fact(facts, 'Binaries in the scan cache', '0', 'cache-count');
  fact(facts, 'Shell', 'Tauri + WebView2');
  fact(facts, 'Runtime dependencies', '0');

  const theme = byId<HTMLSelectElement>('theme');
  theme.value = settings.theme;
  theme.addEventListener('change', () => {
    document.documentElement.dataset['theme'] = theme.value;
    void persist(() => call('settings_set_theme', { theme: theme.value }));
  });

  const group = byId<HTMLInputElement>('group');
  group.checked = settings.groupGamesByStore;
  group.addEventListener('change', () => {
    groupByStore = group.checked;
    renderLibrary();
    void persist(() => call('settings_set_group_games_by_store', { enabled: group.checked }));
  });

  byId('refresh').addEventListener('click', () => void refreshLibrary());
  byId('add-game').addEventListener('click', () => {
    void (async () => {
      const picked = await call<string | null>('pick_folder');
      if (!picked) return;
      await call('library_add_game', { dir: picked });
      await refreshLibrary();
    })();
  });
  byId('scan-close').addEventListener('click', () => {
    byId('scan-panel').hidden = true;
    void call('scan_cancel');
  });

  byId('minimize').addEventListener('click', () => void call('window_minimize'));
  byId('maximize').addEventListener('click', () => void call('window_toggle_maximize'));
  byId('close').addEventListener('click', () => void call('window_close'));

  await refreshLibrary();
  await refreshCacheFact();
}

void main().catch((error: unknown) => {
  byId('health-title').textContent = 'The window failed to start';
  byId('health-detail').textContent =
    error instanceof Error ? error.message : String(error);
  byId('health').hidden = false;
  byId('health').classList.add('warn');
});

// Re-exported so the scan view and library share one definition.
export type { FolderScan };
