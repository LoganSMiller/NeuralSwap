/**
 * Comparing runtime versions.
 *
 * Version strings arrive from three places that do not agree on a format: a
 * PE `VS_FIXEDFILEINFO` block gives four numbers, a PE string table often
 * gives `"310, 8, 0, 0"` with spaces, and a package's own name might carry
 * `3.1.13`. All three have to order correctly against each other, because the
 * answer decides whether an install is an upgrade or a downgrade - and a
 * silent downgrade is the change a user is least likely to expect and most
 * likely to blame on us.
 *
 * Deliberately not semver: these are not semver, and pretending otherwise
 * would mean rejecting `310.8.0.0` as invalid.
 */

/** Numeric components, most significant first. */
export type Version = readonly number[];

/**
 * Parse whatever a PE or a package name offered. Returns `null` when there is
 * no usable number in the string, which is a normal outcome rather than an
 * error - plenty of DLLs carry no version resource at all.
 */
export function parseVersion(raw: string | null | undefined): Version | null {
  if (raw === null || raw === undefined) return null;
  const pieces = raw.split(/[.,\s_-]+/u).filter((part) => part.length > 0);
  // Tested with a regex rather than handed to `parseInt`, which parses a
  // numeric *prefix* and stops: `parseInt('8abc')` is 8, so `310.8abc` would
  // quietly compare as 310.8. A component that is not a plain number makes the
  // whole string unusable rather than partly usable.
  if (pieces.length === 0 || pieces.some((part) => !/^\d+$/u.test(part))) {
    return null;
  }
  return pieces.map((part) => Number.parseInt(part, 10));
}

/**
 * Component-wise ordering, short versions padded with zeroes so that `310.8`
 * and `310.8.0.0` compare equal rather than the shorter one sorting first.
 */
export function compareVersions(left: Version, right: Version): number {
  const width = Math.max(left.length, right.length);
  for (let index = 0; index < width; index += 1) {
    const a = left[index] ?? 0;
    const b = right[index] ?? 0;
    if (a !== b) return a < b ? -1 : 1;
  }
  return 0;
}

/** Canonical dotted form, for display and for the vectors. */
export function formatVersion(version: Version): string {
  return version.join('.');
}

/**
 * How a package version relates to what is already on disk. `unknown` when
 * either side has no parsable version, which the caller must treat as "cannot
 * claim this is an upgrade" rather than as equality.
 */
export type VersionRelation = 'newer' | 'older' | 'same' | 'unknown';

export function relate(
  packageVersion: string | null,
  presentVersion: string | null
): VersionRelation {
  const a = parseVersion(packageVersion);
  const b = parseVersion(presentVersion);
  if (a === null || b === null) return 'unknown';
  const order = compareVersions(a, b);
  if (order > 0) return 'newer';
  if (order < 0) return 'older';
  return 'same';
}
