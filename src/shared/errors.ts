/**
 * Every failure that can reach a user carries a stable machine code. The UI
 * translates the code; the message is only ever developer/diagnostic detail.
 * Upstream threw bare `Error(code)` in some places and `Object.assign` in
 * others, so `err.code` was sometimes the code and sometimes undefined.
 */
/**
 * An array rather than a bare union, so the set exists at run time and can be
 * exported as a vector. `ErrorCode` is derived from it, which means the type
 * and the list cannot disagree - they are the same declaration.
 *
 * They used to be able to disagree with the Rust side, and did:
 * `hardwareUnsupported` was a Rust code with no member here, so the UI could
 * receive a code it had no translation for. `spec/errors.json` now holds both
 * implementations to this list.
 */
export const ERROR_CODES = [
  'unsafePath',
  'reservedName',
  'symlinkRefused',
  'outsideRoot',
  'stateCorrupt',
  'stateUnwritable',
  'stateVersionAhead',
  'jobBusy',
  'jobCancelled',
  'zipInvalid',
  'zipUnsupported',
  'zipTooLarge',
  'zipEntryUnsafe',
  'zipChecksum',
  'peUnreadable',
  'hardwareUnsupported',
  'badRequest',
  // Fetching a component. Separated from the install codes because none of
  // these has touched a game folder: a download that fails leaves nothing
  // behind, and the UI should offer "try again" rather than explain a state.
  'networkFailed',
  'downloadRejected',
  'sourceNotFetchable',
  // Install-time failures. These are the codes that can reach a user while
  // something is being written into a game folder, so each one has to say
  // enough for the UI to explain what state the folder is in.
  'packageInvalid',
  'journalCorrupt',
  'targetLocked',
  'targetProtected',
  'insufficientSpace',
  'verifyFailed',
  'planStale',
  // Anti-cheat is installed with this game. The only refusal whose
  // consequence is irreversible - an injected add-on can get an account
  // banned - so it blocks by default and takes an explicit acknowledgement
  // to get past.
  'antiCheatPresent'
] as const;

export type ErrorCode = (typeof ERROR_CODES)[number];

export class AppError extends Error {
  readonly code: ErrorCode;
  readonly detail: Record<string, unknown>;

  constructor(code: ErrorCode, message?: string, detail: Record<string, unknown> = {}) {
    super(message ?? code);
    this.name = 'AppError';
    this.code = code;
    this.detail = detail;
  }
}

/**
 * Declared as a function rather than an arrow constant so that TypeScript
 * treats it as never-returning for control-flow analysis. A `const fail = ():
 * never =>` does not narrow at the call site, which means every guard written
 * with it still leaves its subject possibly-undefined afterwards.
 */
export function fail(
  code: ErrorCode,
  message?: string,
  detail?: Record<string, unknown>
): never {
  throw new AppError(code, message, detail);
}

export function isAppError(value: unknown): value is AppError {
  return value instanceof AppError;
}

/** Narrow a caught value to a Node errno without `any`. */
export function errnoOf(value: unknown): string | null {
  return typeof value === 'object' && value !== null && 'code' in value
    ? String(value.code)
    : null;
}
