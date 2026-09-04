import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  buildPlan,
  type PackageFile,
  type PlanInput,
  type PresentFile,
  type Step,
  type WarningCode,
} from '../../src/core/install/plan.ts';
import { compareVersions, parseVersion, relate } from '../../src/core/install/version.ts';
import { AppError } from '../../src/shared/errors.ts';

const codeOf = (fn: () => unknown): string => {
  try {
    fn();
  } catch (cause) {
    return cause instanceof AppError ? cause.code : `unexpected:${String(cause)}`;
  }
  return 'no-throw';
};

/** A DLSS runtime as a package would offer it. */
const pkgFile = (over: Partial<PackageFile> = {}): PackageFile => ({
  name: 'nvngx_dlss.dll',
  kind: 'dlss',
  version: '310.8.0.0',
  size: 1000,
  sha256: 'new',
  ...over,
});

const presentFile = (over: Partial<PresentFile> = {}): PresentFile => ({
  rel: 'bin/x64/nvngx_dlss.dll',
  kind: 'dlss',
  version: '310.1.0.0',
  size: 900,
  sha256: 'old',
  managed: false,
  ...over,
});

const input = (over: Partial<PlanInput> = {}): PlanInput => ({
  route: 'nativeDll',
  installDir: 'bin/x64',
  present: [],
  pkg: [pkgFile()],
  ...over,
});

const warningCodes = (plan: { warnings: readonly { code: WarningCode }[] }): WarningCode[] =>
  plan.warnings.map((warning) => warning.code);

const stepFor = (steps: readonly Step[], rel: string): Step => {
  const found = steps.find((step) => step.rel === rel);
  if (found === undefined) throw new Error(`no step for ${rel}`);
  return found;
};

// ------------------------------------------------------------------ versions

test('version strings from every source order against each other', () => {
  // A PE string table writes them with spaces; VS_FIXEDFILEINFO with dots.
  assert.deepEqual(parseVersion('310, 8, 0, 0'), [310, 8, 0, 0]);
  assert.deepEqual(parseVersion('310.8.0.0'), [310, 8, 0, 0]);
  assert.deepEqual(parseVersion('3.1.13'), [3, 1, 13]);
  assert.equal(parseVersion(null), null);
  assert.equal(parseVersion(''), null);
  assert.equal(parseVersion('not a version'), null);
  // A non-numeric component makes the whole string unusable, rather than
  // partly usable. `parseInt` would stop at the numeric prefix and report 8
  // for '8abc', which would let '310.8abc' compare as '310.8'.
  assert.equal(parseVersion('310.8.beta'), null);
  assert.equal(parseVersion('310.8abc'), null);
});

test('shorter versions are padded, not sorted first', () => {
  // The bug this guards: treating a missing component as "less than zero" and
  // reporting 310.8 -> 310.8.0.0 as an upgrade.
  assert.equal(compareVersions([310, 8], [310, 8, 0, 0]), 0);
  assert.equal(compareVersions([310, 8, 1], [310, 8, 0, 0]), 1);
  // Numeric, not lexical: 310 is not less than 9.
  assert.equal(compareVersions([310, 0], [9, 0]), 1);
});

test('an unparsable version on either side is unknown, never equal', () => {
  assert.equal(relate('310.8.0.0', '310.1.0.0'), 'newer');
  assert.equal(relate('310.1.0.0', '310.8.0.0'), 'older');
  assert.equal(relate('310.8.0.0', '310.8'), 'same');
  assert.equal(relate('310.8.0.0', null), 'unknown');
  assert.equal(relate(null, null), 'unknown');
});

// ---------------------------------------------------------------- decisions

test('a file that is not there yet is created, and needs no backup', () => {
  const plan = buildPlan(input());
  assert.equal(plan.steps.length, 1);
  const step = stepFor(plan.steps, 'bin/x64/nvngx_dlss.dll');
  assert.equal(step.action, 'create');
  assert.equal(step.reason, 'newFile');
  assert.equal(step.fromVersion, null);
  assert.equal(step.backupBytes, 0);
  assert.equal(plan.writeBytes, 1000);
  assert.equal(plan.changes, 1);
});

test('identical bytes are skipped, so re-running an install changes nothing', () => {
  const plan = buildPlan(
    input({ present: [presentFile({ sha256: 'new', version: '310.8.0.0', size: 1000 })] })
  );
  const step = stepFor(plan.steps, 'bin/x64/nvngx_dlss.dll');
  assert.equal(step.action, 'skip');
  assert.equal(step.reason, 'identical');
  assert.equal(plan.changes, 0);
  assert.equal(plan.writeBytes, 0);
  assert.ok(warningCodes(plan).includes('nothingToDo'));
});

test('a newer package version is an upgrade and backs the old file up', () => {
  const plan = buildPlan(input({ present: [presentFile()] }));
  const step = stepFor(plan.steps, 'bin/x64/nvngx_dlss.dll');
  assert.equal(step.action, 'replace');
  assert.equal(step.reason, 'upgrade');
  assert.equal(step.fromVersion, '310.1.0.0');
  assert.equal(step.toVersion, '310.8.0.0');
  assert.equal(step.backupBytes, 900);
  assert.equal(plan.backupBytes, 900);
});

test('an older package version is called a downgrade and warned about', () => {
  const plan = buildPlan(
    input({ present: [presentFile({ version: '310.9.0.0' })] })
  );
  assert.equal(stepFor(plan.steps, 'bin/x64/nvngx_dlss.dll').reason, 'downgrade');
  assert.ok(warningCodes(plan).includes('downgrade'));
});

test('same version but different bytes is still a replacement', () => {
  // This is the already-swapped case: somebody put a different build of the
  // same version there. Claiming "already installed" would be wrong.
  const plan = buildPlan(
    input({ present: [presentFile({ version: '310.8.0.0', sha256: 'different' })] })
  );
  const step = stepFor(plan.steps, 'bin/x64/nvngx_dlss.dll');
  assert.equal(step.action, 'replace');
  assert.equal(step.reason, 'sameVersionDifferentBytes');
});

test('an unknown version on either side replaces rather than assuming', () => {
  const plan = buildPlan(input({ present: [presentFile({ version: null })] }));
  assert.equal(stepFor(plan.steps, 'bin/x64/nvngx_dlss.dll').reason, 'versionUnknown');
});

// ----------------------------------------------------------------- warnings

test('replacing a file we did not install is called out', () => {
  const plan = buildPlan(input({ present: [presentFile({ managed: false })] }));
  assert.ok(warningCodes(plan).includes('replacesUnmanagedFile'));

  const ours = buildPlan(input({ present: [presentFile({ managed: true })] }));
  assert.ok(!warningCodes(ours).includes('replacesUnmanagedFile'));
});

test('adding a runtime kind the folder never had is called out', () => {
  // Dropping frame generation into a game that never shipped it is a bigger
  // change than swapping an upscaler, and the user should be told.
  const plan = buildPlan(
    input({
      pkg: [pkgFile({ name: 'sl.dlss_g.dll', kind: 'streamline', version: '2.13.0.0' })],
      present: [presentFile()],
    })
  );
  assert.ok(warningCodes(plan).includes('addsKindNotPresent'));
});

test('a runtime left behind at a mismatched version is called out', () => {
  // Upgrading one DLSS DLL and leaving its sibling at the old version is how a
  // swap turns into a crash on launch.
  const plan = buildPlan(
    input({
      pkg: [pkgFile()],
      present: [
        presentFile(),
        presentFile({ rel: 'bin/x64/nvngx_dlssg.dll', version: '310.1.0.0', sha256: 'g' }),
      ],
    })
  );
  const mixed = plan.warnings.find((warning) => warning.code === 'mixedVersionsAfterInstall');
  assert.ok(mixed !== undefined);
  assert.deepEqual([...mixed.rels].sort(), [
    'bin/x64/nvngx_dlss.dll',
    'bin/x64/nvngx_dlssg.dll',
  ]);
});

test('DLSS and Streamline numbering independently is not a mismatch', () => {
  // 310.8.0.0 beside 2.13.0.0 is correct. Comparing across kinds would make
  // this warning fire on every healthy install, which is worse than useless.
  const plan = buildPlan(
    input({
      pkg: [pkgFile()],
      present: [
        presentFile(),
        presentFile({
          rel: 'bin/x64/sl.dlss.dll',
          kind: 'streamline',
          version: '2.13.0.0',
          sha256: 'sl',
        }),
      ],
    })
  );
  assert.ok(!warningCodes(plan).includes('mixedVersionsAfterInstall'));
});

test('a runtime in another folder is not part of this folder version cohort', () => {
  // The stray copy in the game root that the loader will never look at must
  // not drag the install directory into a false mismatch.
  const plan = buildPlan(
    input({
      present: [
        presentFile({ rel: 'nvngx_dlss.dll', version: '999.0.0.0', sha256: 'stray' }),
      ],
    })
  );
  assert.ok(!warningCodes(plan).includes('mixedVersionsAfterInstall'));
  // And it is not mistaken for the target, either.
  assert.equal(stepFor(plan.steps, 'bin/x64/nvngx_dlss.dll').action, 'create');
});

// -------------------------------------------------------------------- shape

test('the install directory is matched case-insensitively', () => {
  // The scanner reports what the directory entry said; a manifest may disagree
  // about capitalisation. On NTFS they are the same folder.
  const plan = buildPlan(
    input({
      installDir: 'Bin/X64',
      present: [presentFile({ rel: 'bin\\x64\\nvngx_dlss.dll' })],
    })
  );
  assert.equal(stepFor(plan.steps, 'Bin/X64/nvngx_dlss.dll').action, 'replace');
});

test('an empty install directory means the game root', () => {
  const plan = buildPlan(
    input({ installDir: '', present: [presentFile({ rel: 'nvngx_dlss.dll' })] })
  );
  assert.equal(stepFor(plan.steps, 'nvngx_dlss.dll').action, 'replace');
});

test('a package entry that is not a plain file name is refused', () => {
  const bad = (name: string): string =>
    codeOf(() => buildPlan(input({ pkg: [pkgFile({ name })] })));
  assert.equal(bad('../escape.dll'), 'packageInvalid');
  assert.equal(bad('sub/nested.dll'), 'packageInvalid');
  assert.equal(bad('sub\\nested.dll'), 'packageInvalid');
  assert.equal(bad('C:\\Windows\\evil.dll'), 'packageInvalid');
  assert.equal(bad('stream.dll:hidden'), 'packageInvalid');
  assert.equal(bad('evil.dll.'), 'packageInvalid');
  assert.equal(bad('evil.dll '), 'packageInvalid');
  assert.equal(bad('NUL'), 'packageInvalid');
  assert.equal(bad('COM1.dll'), 'packageInvalid');
  assert.equal(bad(''), 'packageInvalid');
  assert.equal(bad(String.fromCharCode(0)), 'packageInvalid');
  // A name that merely contains a reserved word is fine.
  assert.equal(bad('nullify.dll'), 'no-throw');
  // As is one that contains digits resembling an escape.
  assert.equal(bad('nvngx_dlss0000.dll'), 'no-throw');
});

test('an empty or duplicated package is refused', () => {
  assert.equal(codeOf(() => buildPlan(input({ pkg: [] }))), 'packageInvalid');
  assert.equal(
    codeOf(() => buildPlan(input({ pkg: [pkgFile(), pkgFile({ name: 'NVNGX_DLSS.DLL' })] }))),
    'packageInvalid'
  );
});

test('steps come out in a stable order regardless of package order', () => {
  const names = ['sl.dlss.dll', 'nvngx_dlss.dll', 'sl.common.dll'];
  const plan = buildPlan(
    input({ pkg: names.map((name) => pkgFile({ name, sha256: name })) })
  );
  assert.deepEqual(
    plan.steps.map((step) => step.rel),
    ['bin/x64/nvngx_dlss.dll', 'bin/x64/sl.common.dll', 'bin/x64/sl.dlss.dll']
  );
});
