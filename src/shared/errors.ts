/**
 * Every failure that can reach a user carries a stable machine code. The UI
 * translates the code; the message is only ever developer/diagnostic detail.
 * Upstream threw bare `Error(code)` in some places and `Object.assign` in
 * others, so `err.code` was sometimes the code and sometimes undefined.
 */
export type ErrorCode =
  | 'unsafePath'
  | 'reservedName'
  | 'symlinkRefused'
  | 'outsideRoot'
  | 'stateCorrupt'
  | 'stateUnwritable'
  | 'stateVersionAhead'
  | 'jobBusy'
  | 'jobCancelled'
  | 'zipInvalid'
  | 'zipUnsupported'
  | 'zipTooLarge'
  | 'zipEntryUnsafe'
  | 'zipChecksum'
  | 'peUnreadable'
  | 'ipcBadRequest';

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
