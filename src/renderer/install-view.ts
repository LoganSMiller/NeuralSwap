import { badge, fileSize } from './bridge.ts';
import { CHECK_LABELS, type CheckName } from '../shared/checks.ts';
import type {
  Check,
  Integrity,
  InstallOutcome,
  Plan,
  PlanReply,
  RestoreOutcome,
  Step,
} from './types.ts';

/**
 * The plan screen exists so that nothing is a surprise. Every one of these
 * strings answers "what is about to happen to my game folder, and why", in
 * terms somebody can act on rather than in the vocabulary of the code.
 *
 * The codes are the contract; this file is the only place that turns them into
 * sentences, so a change of wording touches one file and a change of meaning
 * fails a test on the Rust side.
 */
const REASONS: Record<Step['reason'], string> = {
  newFile: 'new file — nothing to replace',
  identical: 'already exactly this file',
  upgrade: 'newer version',
  downgrade: 'older version than what is there now',
  sameVersionDifferentBytes: 'same version number, different file',
  versionUnknown: 'version could not be read',
};

const WARNINGS: Record<string, { title: string; why: string }> = {
  downgrade: {
    title: 'This installs an older version',
    why: 'The files already in the folder are newer than the ones in this package. That may be what you want, but it is worth knowing.',
  },
  replacesUnmanagedFile: {
    title: 'This replaces files NeuralSwap did not install',
    why: 'These are either the game’s own files or ones you put there yourself. A copy of each is kept, so this can be undone.',
  },
  addsKindNotPresent: {
    title: 'This adds something the game did not ship',
    why: 'The folder has no runtime of this kind at the moment. Adding one is a bigger change than swapping an existing file, and the game may not use it.',
  },
  mixedVersionsAfterInstall: {
    title: 'Some files would be left at a different version',
    why: 'A game installs its runtimes as a matched set. Leaving one behind at another version is a common cause of a crash on launch — consider a package that covers all of them.',
  },
  nothingToDo: {
    title: 'Nothing to do',
    why: 'Every file in this package is already exactly what is in the folder.',
  },
};


const FILE_STATUS: Record<string, { label: string; why: string }> = {
  intact: {
    label: 'intact',
    why: 'Still exactly the file NeuralSwap wrote.',
  },
  changed: {
    label: 'changed since install',
    why: 'This is no longer the file NeuralSwap wrote. A game update is the usual cause, and it means the swap is no longer in effect.',
  },
  missing: {
    label: 'missing',
    why: 'The file NeuralSwap wrote is no longer there.',
  },
  unreadable: {
    label: 'could not be read',
    why: 'The file is there but could not be opened to check it.',
  },
};

const RESTORE_ACTIONS: Record<string, string> = {
  restoredOriginal: 'original put back',
  removedOurs: 'removed',
  leftAlone: 'left as it is',
  failed: 'could not be done',
};

function heading(level: 'h2' | 'h3', text: string): HTMLHeadingElement {
  const node = document.createElement(level);
  node.textContent = text;
  return node;
}

function paragraph(text: string, className?: string): HTMLParagraphElement {
  const node = document.createElement('p');
  node.textContent = text;
  if (className !== undefined) node.className = className;
  return node;
}

function versionArrow(step: Step): string {
  const from = step.fromVersion ?? '—';
  const to = step.toVersion ?? '—';
  if (step.action === 'skip') return to;
  if (step.fromVersion === null && step.action === 'create') return to;
  return `${from} → ${to}`;
}

function stepRow(step: Step): HTMLTableRowElement {
  const row = document.createElement('tr');
  if (step.action === 'skip') row.classList.add('muted-row');

  const name = document.createElement('th');
  name.scope = 'row';
  name.textContent = step.rel;
  if (step.action === 'replace') {
    name.append(
      ' ',
      badge('backed up', 'A copy of the current file is kept so this can be undone.')
    );
  }

  const action = document.createElement('td');
  action.textContent =
    step.action === 'create' ? 'add' : step.action === 'replace' ? 'replace' : 'skip';

  const version = document.createElement('td');
  version.textContent = versionArrow(step);

  const why = document.createElement('td');
  why.textContent = REASONS[step.reason];

  const size = document.createElement('td');
  size.className = 'numeric';
  size.textContent = step.action === 'skip' ? '—' : fileSize(step.writeBytes);

  row.append(name, action, version, why, size);
  return row;
}

function planTable(plan: Plan): HTMLElement {
  const wrapper = document.createElement('div');
  wrapper.className = 'table-scroll';

  const table = document.createElement('table');
  table.className = 'plan-table';

  const head = document.createElement('thead');
  const headRow = document.createElement('tr');
  for (const label of ['File', 'Action', 'Version', 'Why', 'Size']) {
    const cell = document.createElement('th');
    cell.scope = 'col';
    cell.textContent = label;
    headRow.append(cell);
  }
  head.append(headRow);

  const body = document.createElement('tbody');
  for (const step of plan.steps) body.append(stepRow(step));

  table.append(head, body);
  wrapper.append(table);
  return wrapper;
}

function warningList(plan: Plan): HTMLElement | null {
  const relevant = plan.warnings.filter((warning) => warning.code !== 'nothingToDo');
  if (relevant.length === 0) return null;

  const list = document.createElement('ul');
  list.className = 'warnings';
  for (const warning of relevant) {
    const item = document.createElement('li');
    const known = WARNINGS[warning.code];
    const title = document.createElement('strong');
    title.textContent = known?.title ?? warning.code;
    item.append(title);
    if (known !== undefined) item.append(' ', document.createTextNode(known.why));
    if (warning.rels.length > 0) {
      const files = document.createElement('div');
      files.className = 'warning-files';
      files.textContent = warning.rels.join(', ');
      item.append(files);
    }
    list.append(item);
  }
  return list;
}

function checkRow(check: Check): HTMLLIElement {
  const item = document.createElement('li');
  item.className = `check check-${check.outcome}`;

  const label = document.createElement('strong');
  label.textContent = CHECK_LABELS[check.name as CheckName] ?? check.name;
  item.append(label);

  // Every check is listed, including the ones that passed. A user who can see
  // the whole list knows what was looked at, rather than being told only about
  // whichever obstacle happened to be discovered first.
  const outcome = document.createElement('span');
  outcome.className = 'check-outcome';
  outcome.textContent =
    check.outcome === 'pass'
      ? 'ok'
      : check.outcome === 'fail'
        ? 'blocked'
        : check.outcome === 'warn'
          ? 'note'
          : 'not checked';
  item.append(' ', outcome);

  if (check.detail !== '') item.append(paragraph(check.detail, 'check-detail'));
  return item;
}

export interface PlanHandlers {
  onInstall: () => void;
  onCancel: () => void;
}

export function renderPlan(
  host: HTMLElement,
  reply: PlanReply,
  handlers: PlanHandlers
): void {
  host.replaceChildren();

  const plan = reply.plan;
  host.append(heading('h2', 'What will change'));

  const into = plan.installDir === '' ? 'the game folder' : plan.installDir;
  if (plan.changes === 0) {
    host.append(
      paragraph(
        `Everything in this package is already in ${into}. Installing would change nothing.`
      )
    );
  } else {
    host.append(
      paragraph(
        `${plan.changes} file${plan.changes === 1 ? '' : 's'} in ${into} — ` +
          `${fileSize(plan.writeBytes)} written, ${fileSize(plan.backupBytes)} copied aside first.`
      )
    );
  }

  host.append(planTable(plan));

  const warnings = warningList(plan);
  if (warnings !== null) host.append(warnings);

  host.append(heading('h3', 'Checks'));
  const checks = document.createElement('ul');
  checks.className = 'checks';
  for (const check of reply.preflight.checks) checks.append(checkRow(check));
  host.append(checks);

  const actions = document.createElement('div');
  actions.className = 'row-actions';

  const install = document.createElement('button');
  install.type = 'button';
  install.className = 'primary';
  install.textContent = plan.changes === 0 ? 'Nothing to install' : 'Install';
  // Disabled rather than hidden: a button that vanishes leaves a user
  // wondering whether they missed it.
  install.disabled = !reply.preflight.ok || plan.changes === 0 || reply.busy;
  if (reply.busy) install.title = 'Something is already running for this game.';
  else if (!reply.preflight.ok) install.title = 'One of the checks above has to pass first.';
  install.addEventListener('click', handlers.onInstall);

  const cancel = document.createElement('button');
  cancel.type = 'button';
  cancel.textContent = 'Close';
  cancel.addEventListener('click', handlers.onCancel);

  actions.append(install, cancel);
  host.append(actions);
}

/** What happened. Stated plainly, especially when it went wrong. */
export function renderOutcome(host: HTMLElement, outcome: InstallOutcome): void {
  host.replaceChildren();

  if (outcome.outcome === 'installed') {
    host.append(heading('h2', 'Installed'));
    if (outcome.installed.length > 0) {
      host.append(
        paragraph(
          `${outcome.installed.length} file${outcome.installed.length === 1 ? '' : 's'} written ` +
            `(${fileSize(outcome.bytesWritten)}). The files that were there before have been kept, ` +
            `so this can be undone.`
        )
      );
      const list = document.createElement('ul');
      list.className = 'plain-list';
      for (const rel of outcome.installed) {
        const item = document.createElement('li');
        item.textContent = rel;
        list.append(item);
      }
      host.append(list);
    } else {
      host.append(paragraph('Nothing needed changing.'));
    }
    return;
  }

  if (outcome.outcome === 'refused') {
    host.append(heading('h2', 'Not started'));
    host.append(paragraph('One of the checks did not pass, so nothing was written.'));
    const checks = document.createElement('ul');
    checks.className = 'checks';
    for (const check of outcome.checks) checks.append(checkRow(check));
    host.append(checks);
    return;
  }

  // Failed. The first thing a user needs is the state of their game folder,
  // not the error code, so that is what leads.
  const state =
    outcome.reached === 'nothingWritten'
      ? 'Nothing was written — your game folder was not touched.'
      : outcome.reached === 'rolledBack'
        ? 'Everything that had been written has been put back. Your game folder is as it was.'
        : 'Some files were changed and could not be put back automatically.';

  host.append(heading('h2', 'Install did not finish'));
  const summary = paragraph(state);
  summary.className =
    outcome.reached === 'partiallyApplied' ? 'callout danger' : 'callout';
  host.append(summary);
  host.append(paragraph(outcome.message, 'check-detail'));

  if (outcome.rollbackFailures.length > 0) {
    host.append(
      paragraph(
        'NeuralSwap will try again the next time it starts. Until then, please do not ' +
          'reinstall over this folder.'
      )
    );
    const list = document.createElement('ul');
    list.className = 'plain-list';
    for (const failure of outcome.rollbackFailures) {
      const item = document.createElement('li');
      item.textContent = failure;
      list.append(item);
    }
    host.append(list);
  }
}

/** Whether what we installed is still there. */
export function renderStatus(host: HTMLElement, status: Integrity | null): void {
  host.replaceChildren();

  if (status === null) {
    host.append(paragraph('NeuralSwap has not installed anything in this game.'));
    return;
  }

  host.append(heading('h3', 'Installed by NeuralSwap'));
  if (status.intact) {
    host.append(paragraph('Every file is still exactly as installed.'));
  } else {
    host.append(
      paragraph(
        'Some files are no longer as installed. A game update is the usual reason.',
        'callout'
      )
    );
  }

  const list = document.createElement('ul');
  list.className = 'plain-list';
  for (const file of status.files) {
    const item = document.createElement('li');
    item.textContent = file.rel;
    const known = FILE_STATUS[file.status];
    if (known !== undefined && file.status !== 'intact') {
      item.append(' ', badge(known.label, known.why));
    }
    if (!file.restorable) {
      item.append(
        ' ',
        badge(
          'no saved original',
          'There is no kept copy of what was here before, so this file cannot be put back.',
          true
        )
      );
    }
    list.append(item);
  }
  host.append(list);
}

export function renderRestore(host: HTMLElement, outcome: RestoreOutcome): void {
  host.replaceChildren();

  if (outcome.outcome === 'nothingInstalled') {
    host.append(paragraph('There is nothing for NeuralSwap to undo in this game.'));
    return;
  }

  host.append(heading('h2', outcome.complete ? 'Put back' : 'Partly put back'));
  if (!outcome.complete) {
    host.append(
      paragraph(
        'Some files could not be dealt with. The install record has been kept, so this ' +
          'can be tried again.',
        'callout'
      )
    );
  }

  const list = document.createElement('ul');
  list.className = 'plain-list';
  for (const file of outcome.files) {
    const item = document.createElement('li');
    item.textContent = file.rel;
    item.append(' ', badge(RESTORE_ACTIONS[file.action] ?? file.action, file.detail));
    if (file.action === 'leftAlone' || file.action === 'failed') {
      item.append(paragraph(file.detail, 'check-detail'));
    }
    list.append(item);
  }
  host.append(list);
}
