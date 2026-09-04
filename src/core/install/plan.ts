/**
 * Deriving an install plan.
 *
 * This is a pure function, and that is the point. Everything a user is shown
 * before they agree to an install - every path, every version transition,
 * every warning - is decided here, from data, with no filesystem access. So
 * the dry run and the real run cannot disagree about what is about to happen:
 * the real run is handed this same structure and does exactly what it says.
 *
 * Upstream decided as it copied. A file was inspected, judged and overwritten
 * in one pass, which meant the only way to find out what an install would do
 * was to let it happen, and a refusal half-way left the folder in a state
 * nobody had described.
 *
 * The shape checks here are lexical only. Proving a path really stays inside
 * the game folder needs the filesystem (a junction is not visible in a
 * string), so that check lives in `apply` where the write happens.
 */
import { fail } from '../../shared/errors.ts';
import { relate } from './version.ts';

export type RuntimeKind = 'dlss' | 'streamline';

/** For now the one route: drop the DLLs beside the executable that loads them. */
export type Route = 'nativeDll';

export type PackageFile = {
  readonly name: string;
  readonly kind: RuntimeKind;
  readonly version: string | null;
  readonly size: number;
  readonly sha256: string;
};

export type PresentFile = {
  readonly rel: string;
  readonly kind: RuntimeKind;
  readonly version: string | null;
  readonly size: number;
  readonly sha256: string;
  /** Recorded in our own install manifest, so we know we put it there. */
  readonly managed: boolean;
};

export type PlanInput = {
  readonly route: Route;
  /** Directory inside the game to install into. Empty string is the game root. */
  readonly installDir: string;
  readonly present: readonly PresentFile[];
  readonly pkg: readonly PackageFile[];
};

export type StepAction = 'create' | 'replace' | 'skip';

/**
 * Why a step does what it does. Stable machine strings: the UI translates
 * them and the vectors assert them.
 */
export type StepReason =
  | 'newFile'
  | 'identical'
  | 'upgrade'
  | 'downgrade'
  | 'sameVersionDifferentBytes'
  | 'versionUnknown';

export type Step = {
  readonly rel: string;
  readonly action: StepAction;
  readonly reason: StepReason;
  readonly kind: RuntimeKind;
  readonly fromVersion: string | null;
  readonly toVersion: string | null;
  /** Bytes the new file occupies. Zero for a skip. */
  readonly writeBytes: number;
  /** Bytes that must be copied aside first. Zero unless something is replaced. */
  readonly backupBytes: number;
  readonly sha256: string;
};

export type WarningCode =
  | 'downgrade'
  | 'replacesUnmanagedFile'
  | 'addsKindNotPresent'
  | 'mixedVersionsAfterInstall'
  | 'nothingToDo';

export type Warning = {
  readonly code: WarningCode;
  readonly rels: readonly string[];
};

export type Plan = {
  readonly route: Route;
  readonly installDir: string;
  readonly steps: readonly Step[];
  readonly warnings: readonly Warning[];
  readonly writeBytes: number;
  readonly backupBytes: number;
  /** Steps that actually change the folder. Zero means already installed. */
  readonly changes: number;
};

const RESERVED_STEMS = ['con', 'prn', 'aux', 'nul'];

/**
 * Built rather than written as an escape. A literal NUL in a source file makes
 * the file binary to every text tool that touches it - `grep` stops reporting
 * matches and reports "binary file matches" instead - and the escape sequence
 * that would avoid that is itself easy to mangle in transit.
 */
const NUL = String.fromCharCode(0);

function isReserved(name: string): boolean {
  const stem = (name.split('.')[0] ?? name).toLowerCase();
  if (RESERVED_STEMS.includes(stem)) return true;
  return /^(?:com|lpt)[0-9]$/.test(stem);
}

/**
 * A package entry must be a plain file name. Anything with a separator in it
 * is either a package we do not understand or an attempt to write outside the
 * install directory, and neither is worth guessing about.
 */
function assertPlainFileName(name: string): void {
  if (name.length === 0) fail('packageInvalid', 'package entry has an empty name');
  if (name.includes('/') || name.includes('\\')) {
    fail('packageInvalid', `package entry is not a plain file name: ${name}`);
  }
  if (name.includes(':')) fail('packageInvalid', `colon in package entry: ${name}`);
  if (name.includes(NUL)) fail('packageInvalid', 'NUL byte in package entry');
  if (name === '.' || name === '..') fail('packageInvalid', `package entry is a dot name: ${name}`);
  if (name.endsWith('.') || name.endsWith(' ')) {
    fail('packageInvalid', `trailing dot or space in package entry: ${name}`);
  }
  if (isReserved(name)) fail('packageInvalid', `DOS device name in package: ${name}`);
}

/** Forward slashes, so a plan reads the same however the scanner spelled it. */
function joinRel(dir: string, name: string): string {
  const clean = dir.replace(/[\\/]+$/u, '');
  return clean.length === 0 ? name : `${clean.replace(/\\/gu, '/')}/${name}`;
}

/** Comparison key. Windows filesystems are case-insensitive; treat them so. */
function relKey(rel: string): string {
  return rel.replace(/\\/gu, '/').toLowerCase();
}

function parentKey(rel: string): string {
  const key = relKey(rel);
  const cut = key.lastIndexOf('/');
  return cut < 0 ? '' : key.slice(0, cut);
}

/** The install directory as a comparison key, so it can be matched against
 * `parentKey` of a scanned file. Trailing separators and casing vary. */
function dirKeyOf(installDir: string): string {
  return relKey(installDir).replace(/\/+$/u, '');
}

function decide(pkg: PackageFile, present: PresentFile | undefined): {
  action: StepAction;
  reason: StepReason;
} {
  if (present === undefined) return { action: 'create', reason: 'newFile' };
  // Byte equality first: it is the only comparison that is certainly right,
  // and it is what makes re-running an install a no-op instead of a rewrite.
  if (present.sha256 === pkg.sha256) return { action: 'skip', reason: 'identical' };
  switch (relate(pkg.version, present.version)) {
    case 'newer':
      return { action: 'replace', reason: 'upgrade' };
    case 'older':
      return { action: 'replace', reason: 'downgrade' };
    case 'same':
      // Same version, different bytes. Somebody has already swapped this file,
      // or it was built differently. Worth replacing, worth backing up.
      return { action: 'replace', reason: 'sameVersionDifferentBytes' };
    case 'unknown':
      // Listed rather than left to a `default`, so that adding a relation to
      // the union fails the build instead of quietly landing here.
      return { action: 'replace', reason: 'versionUnknown' };
  }
}

/**
 * What each runtime version in the install directory will be once the plan has
 * run. Compared per kind, because DLSS and Streamline number independently -
 * `310.8.0.0` beside `2.13.0.0` is correct, and flagging it would be noise.
 */
function mixedVersions(
  input: PlanInput,
  steps: readonly Step[],
  dirKey: string
): readonly string[] {
  const touched = new Set(steps.map((step) => relKey(step.rel)));
  const byKind = new Map<RuntimeKind, Map<string, string[]>>();

  const note = (kind: RuntimeKind, version: string | null, rel: string) => {
    if (version === null) return;
    const versions = byKind.get(kind) ?? new Map<string, string[]>();
    const rels = versions.get(version) ?? [];
    rels.push(rel);
    versions.set(version, rels);
    byKind.set(kind, versions);
  };

  for (const step of steps) {
    note(step.kind, step.action === 'skip' ? step.fromVersion : step.toVersion, step.rel);
  }
  // Files already in the folder that the package says nothing about. These are
  // the ones that get left behind at an old version and then crash the game.
  for (const file of input.present) {
    if (parentKey(file.rel) !== dirKey) continue;
    if (touched.has(relKey(file.rel))) continue;
    note(file.kind, file.version, file.rel);
  }

  const offenders: string[] = [];
  for (const versions of byKind.values()) {
    if (versions.size < 2) continue;
    for (const rels of versions.values()) offenders.push(...rels);
  }
  return offenders.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
}

export function buildPlan(input: PlanInput): Plan {
  if (input.pkg.length === 0) fail('packageInvalid', 'package contains no runtime files');

  const seen = new Set<string>();
  for (const file of input.pkg) {
    assertPlainFileName(file.name);
    if (file.size < 0) fail('packageInvalid', `negative size for ${file.name}`);
    const key = file.name.toLowerCase();
    if (seen.has(key)) fail('packageInvalid', `duplicate package entry: ${file.name}`);
    seen.add(key);
  }

  const byRel = new Map<string, PresentFile>();
  for (const file of input.present) byRel.set(relKey(file.rel), file);

  const dirKey = dirKeyOf(input.installDir);
  const kindsPresentInDir = new Set<RuntimeKind>();
  for (const file of input.present) {
    if (parentKey(file.rel) === dirKey) kindsPresentInDir.add(file.kind);
  }

  const steps: Step[] = [];
  for (const file of input.pkg) {
    const rel = joinRel(input.installDir, file.name);
    const present = byRel.get(relKey(rel));
    const { action, reason } = decide(file, present);
    steps.push({
      rel,
      action,
      reason,
      kind: file.kind,
      fromVersion: present?.version ?? null,
      toVersion: file.version,
      writeBytes: action === 'skip' ? 0 : file.size,
      backupBytes: action === 'replace' ? (present?.size ?? 0) : 0,
      sha256: file.sha256,
    });
  }
  steps.sort((a, b) => (relKey(a.rel) < relKey(b.rel) ? -1 : 1));

  const collect = (test: (step: Step) => boolean): readonly string[] =>
    steps.filter(test).map((step) => step.rel);

  const warnings: Warning[] = [];
  const downgrades = collect((step) => step.reason === 'downgrade');
  if (downgrades.length > 0) warnings.push({ code: 'downgrade', rels: downgrades });

  // A replacement of something we did not install is the case worth stating
  // plainly: it may be the game's own file, or a swap the user did by hand and
  // has forgotten. The backup makes it reversible either way, but they should
  // be told before it happens, not after.
  const unmanaged = collect(
    (step) => step.action === 'replace' && byRel.get(relKey(step.rel))?.managed !== true
  );
  if (unmanaged.length > 0) warnings.push({ code: 'replacesUnmanagedFile', rels: unmanaged });

  const novel = collect(
    (step) => step.action === 'create' && !kindsPresentInDir.has(step.kind)
  );
  if (novel.length > 0) warnings.push({ code: 'addsKindNotPresent', rels: novel });

  const mixed = mixedVersions(input, steps, dirKey);
  if (mixed.length > 0) warnings.push({ code: 'mixedVersionsAfterInstall', rels: mixed });

  const changes = steps.filter((step) => step.action !== 'skip').length;
  if (changes === 0) warnings.push({ code: 'nothingToDo', rels: [] });

  const sum = (pick: (step: Step) => number): number =>
    steps.reduce((total, step) => total + pick(step), 0);

  return {
    route: input.route,
    installDir: input.installDir,
    steps,
    warnings,
    writeBytes: sum((step) => step.writeBytes),
    backupBytes: sum((step) => step.backupBytes),
    changes,
  };
}
