# NeuralSwap

[![CI](https://github.com/LoganSMiller/NeuralSwap/actions/workflows/ci.yml/badge.svg)](https://github.com/LoganSMiller/NeuralSwap/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/LoganSMiller/NeuralSwap?include_prereleases&label=release)](https://github.com/LoganSMiller/NeuralSwap/releases/latest)
[![License](https://img.shields.io/badge/licence-MIT-blue)](LICENSE)

Neural-rendering upscaler management for PC games: find the games already
installed, show exactly what will change, install, verify, and put it back.

Built after a close read of [rakanki911/DLSS5-Swapper][upstream], which solves
the same problem and solves a lot of it well. This is a separate codebase, not
a fork — see [ATTRIBUTION.md](ATTRIBUTION.md) for what it owes that project and
for the third-party components the install routes orchestrate.

**Status: the whole reference core is ported to Rust; library discovery,
folder scanning and the native DLL swap work end to end against real installs
— plan, preflight, write-ahead journal, verification and restore, with a
screen in front of them.** What is missing is the other two routes (ReShade +
Feeder, OptiScaler) and any notion of where packages come from: you point it
at a folder of runtime DLLs you already have.

The section at the bottom says precisely what exists and what does not.
Nothing here is claimed to work that has not been run.

---

## Shape

Rust + Tauri 2, rendering in the WebView2 already present on Windows.

```
spec/                     behavioural vectors - the contract both languages meet
src-tauri/
  src/                    the Tauri app: commands, argument types, scanner state
  crates/core/            host-agnostic core - no Tauri, no UI
    src/error.rs          stable machine codes
    src/bytes.rs          bounds-checked little-endian reads
    src/hash.rs           content hashing - the only honest identity for a file
    src/fsx/              path safety, durable writes
    src/zip/              hardened archive extraction
    src/pe/               PE inspection and its cache
    src/jobs/             locks and cancellable parallel sweeps
    src/scan/             API detection, candidate ranking, folder walk
    src/install/          plan, preflight, journal, manifest, apply, restore
    src/library/          Steam, Epic and Xbox discovery, and a VDF reader
    src/platform/         the few things that must ask Windows itself
    src/settings/         schema, migrations, sanitiser, store
    examples/             point the scanner or the library at a real machine
    tests/                replays spec/, plus end-to-end scanning and installing
src/                      the TypeScript reference implementation (see below)
  renderer/               the frontend, which is shipped
test/unit/                the reference implementation's tests
```

### Why there are two implementations

`src/` holds a complete TypeScript implementation of the same core. It is not
dead code and it is not shipped: it is the **reference**, and it is what
`spec/` is generated from.

The largest risk in reimplementing a security-critical core in another language
is silently dropping an edge case. That is not hypothetical — while building
the TypeScript version, a lost escape in a character class reduced `[\\/]` to
`[\/]`, which let every backslash-separated `..` skip segment validation
entirely. A hand-written test caught it in under a minute.

A port cannot inherit those tests, so it inherits the vectors instead:
`npm run vectors` writes 106 cases as JSON tables plus byte-identical binary
fixtures, and `cargo test` replays them against the Rust core. "Did the port
preserve the rules?" becomes a test run rather than a judgement call. CI fails
if regenerating produces anything other than what is committed.

The install planner and the journal-recovery decision were written this way
round on purpose — reference first, vectors, then Rust — because they are the
rules that govern somebody else's game folder. The 47 cases covering them are
compared whole: a reason, a byte total or a warning that differs counts as a
divergence even when the file list agrees, since those are the parts a user
actually reads.

[What building it twice caught](#what-building-it-twice-caught), at the bottom,
is the list of defects this has actually found.

---

## What is better than upstream, and by how much

### Dependencies

Zero production npm dependencies. The core crate's direct Rust dependencies
are `serde`, `serde_json`, `flate2` (pure-Rust `miniz_oxide` backend),
`crc32fast`, `memchr`, `sha2`, and on Windows `windows-registry` and
`windows-sys`. All pure Rust bar the two Microsoft crates, so the build needs
no native toolchain beyond the linker; `npm audit --omit=dev` and `cargo
audit` both run in CI.

Upstream has exactly one production dependency, `extract-zip@2.0.1`, which
carries [GHSA-jmr9-qjv8-65gv][advisory] — unvalidated symlink path traversal —
across its entire published range, with no fixed version to upgrade to. It is
used to unpack archives fetched over the network into the user's profile.

[`crates/core/src/zip`](src-tauri/crates/core/src/zip) replaces it: symlink
entries are refused outright rather than resolved, every entry name goes
through the path validator, entry count and decompressed size are capped before
inflating, and every entry's CRC-32 and length are checked against the central
directory as it is written. The vectors include the advisory's exact shape — a
symlink entry followed by a write through it — and a test asserts the file it
aimed at does not appear anywhere, not merely that an error came back.

### Footprint

Measured on this machine, release build:

| | upstream (Electron 33) | NeuralSwap (Tauri 2) |
| --- | --- | --- |
| **Installer** | 230.8 MB | **1.26 MB** |
| Application binary | — | 3.29 MB |
| Frontend bundle | — | 23.1 kB |
| Memory, own process tree | — | 354 MB |

The installer is the real win: **183x smaller**, because WebView2 is already
part of Windows rather than a bundled copy of Chromium. The caveat is that the
installer uses the download bootstrapper, so on a machine without WebView2 it
fetches the runtime on first install — present by default on Windows 11.

These numbers grew with the install engine, and it is worth being straight
about the cost: the installer went from 1.1 MB to 1.26 MB and the binary from
2.9 MB to 3.29 MB, which is what a SHA-256 implementation, the journal, the
manifest and the restore path weigh. The frontend went from 9.0 kB to 23.1 kB,
most of it the plan and preflight screens — a bigger proportional jump, and
the right trade for a screen whose entire job is telling somebody what is
about to happen to their game folder.

The memory figure is deliberately not presented as a win. 354 MB is this app's
own tree — one `neuralswap.exe` plus six WebView2 processes — measured by
walking `ParentProcessId`, not by counting every `msedgewebview2.exe` on the
machine. There were nineteen of those, because Windows Widgets and Outlook use
WebView2 too; an earlier measurement that counted all of them reported 1,578 MB
and was meaningless. WebView2 *is* a full Chromium, so leaving Electron buys a
much smaller download and a single small signable artefact — not a
fundamentally lighter runtime.

### Library discovery

Each storefront is asked about its own installs rather than guessed at, so a
game moved to a second drive is still found:

- **Steam** — `libraryfolders.vdf` for every library root, then one
  `appmanifest_*.acf` per app for its id, name and folder. Needs a small
  KeyValues reader, which is [`library/vdf.rs`](src-tauri/crates/core/src/library/vdf.rs).
- **Epic** — the JSON manifests under `%ProgramData%`, which name the install
  location exactly; Epic lets a title be installed on any drive.
- **Xbox / Game Pass** — the flat-file `XboxGames` folders, with the display
  name read out of `MicrosoftGame.config`. The older `WindowsApps` layout is
  not readable without taking ownership, which this will never do.

On a real machine: **11 games in 1.8 ms**, with proper titles
(`EscapeTheBackrooms` → "Escape the Backrooms", `Halo- Campaign Evolved` →
"Halo: Campaign Evolved").

Three things that only show up against real data:

- Steam's **`bIsIncompleteInstall`** uses Unreal's lower-case `b` prefix, which
  `PascalCase` renders as `BIsIncompleteInstall` and never matches — so every
  half-downloaded title would have been listed as installed. A test caught it.
- **Steamworks Common Redistributables** installs like a game and has an
  ordinary manifest. Offering it as a target means somebody patching a shared
  runtime every other game depends on. Proton and the Linux runtimes are
  matched by name rather than id, because there is a new app id every release.
- A manifest can **outlive the folder** it names, so the directory is checked
  before a game is offered.

### The scanner

Points at a game folder, reads executable headers, and reports every candidate
best-first with its graphics API — plus what it deliberately skipped, because
"why didn't it find my game" needs an answer.

Measured on a real Cyberpunk 2077 install (release build):

| | |
| --- | --- |
| Cold scan | **51 ms** (walk 2 ms, 4 binaries parsed 47 ms) |
| Rescan, nothing changed | **5 ms**, 0 binaries re-read |

Getting there found three real bugs, in this code:

- **The cache was write-only.** `scan_folder` took `&mut PeCache`, so it could
  not share it with the worker threads: it parsed everything and wrote the
  results in afterwards. A rescan cost exactly as much as the first scan, which
  is the opposite of the point of having a cache. It now takes the `Mutex` and
  consults it from the pool, locking only around each lookup and insert.
- **The marker search was hand-rolled.** `haystack.windows(n).position(..)` per
  marker per chunk took **six seconds** on Cyberpunk's 58 MB executable.
  `memchr::memmem` is SIMD-accelerated and was already in the dependency tree.
- **Every file was being stat'ed.** The walk built a relative path and read
  metadata for all ~240 entries before deciding it only cared about four.

`ScanStats` reports the walk and parse cost separately, which is what localised
each of these in one run rather than by guesswork.

### Telling a hand-placed runtime from a shipped one

The presence of an `nvngx` DLL is **not** proof that a game has native NGX
calls — somebody may have copied one in. Offering the native install route on
that basis produces an install that cannot work.

Modification time was tried first and does not work: Windows `CopyFile`
preserves the source timestamp, so a DLL taken from an NVIDIA package keeps its
original build date and looks *older* than the game rather than newer. Tested
against a real install with a known hand-placed file, every runtime came back
indistinguishable.

Version cohorts do work, because a game installs its runtimes as a matched set.
On the same install, with ground truth from the person who put the file there:

```
Dlss  ConsistentWithSiblings      bin/x64/nvngx_dlss.dll     v310.1.0.0
Dlss  ConsistentWithSiblings      bin/x64/nvngx_dlssg.dll    v310.1.0.0
Dlss  VersionDiffersFromSiblings  bin/x64/nvngx_dlssnr.dll   v310.8.0.0  <- added by hand
Dlss  NotBesideExecutable         nvngx_dlss.dll             v310.8.0.0
```

The verdicts name the *evidence*, not a conclusion: nothing here can know what
a developer shipped. The authoritative answers are our own install manifest
(for files we placed) and an Authenticode check (for whether a file is a
genuine NVIDIA build); neither exists yet.

### Path safety

[`crates/core/src/fsx/paths.rs`](src-tauri/crates/core/src/fsx/paths.rs)
refuses traversal, rooted and UNC paths, alternate data streams, DOS device
names (`CON`, `NUL`, `COM1`, `LPT1.txt` — while allowing `console.dll` and
`nullify.txt`), trailing-dot and trailing-space ambiguity, NUL bytes, and any
path crossing an existing symlink or junction.

The rules are decided without consulting the host platform's path module. The
TypeScript version leaned on Node's `path.isAbsolute`, so a UNC path was
refused on Windows and quietly accepted as an odd filename on Linux. The Rust
port makes both platforms agree, on the strict answer, and a test asserts it.

### Settings that survive

[`crates/core/src/settings`](src-tauri/crates/core/src/settings) is versioned,
migrating and atomic:

- A corrupt file falls back to the previous good copy; if that is unusable too,
  the wreckage is moved aside under a timestamped name and **never deleted**.
- Settings written by a *newer* build are refused rather than downgraded, and
  the file is left byte-for-byte untouched.
- One malformed field costs you that field, not the other forty.
- Write failures are surfaced with the reason, and shown in the window.
- The Windows case where Defender or the search indexer briefly holds a file
  mid-replace is retried rather than reported as failure. Not hypothetical: it
  appeared as intermittent `EPERM` while building the reference.

Upstream's loader returns blank defaults on any read failure and its writer is
a bare `writeFileSync` inside `try {} catch {}` — so a torn write means a user
finds every scan folder, hidden game, custom poster and language reset one
morning, with no indication anything happened.

### The boundary

Tauri deserialises command arguments with serde, so argument validation lives
in the type: a command taking an `AbsolutePath`
([`src-tauri/src/validate.rs`](src-tauri/src/validate.rs)) cannot be reached
with a relative path, a device-namespace path, or a string with a NUL in it,
and no wrapper has to remember to check. Errors cross as `{ code, message }`
with a stable code; the code is the contract, the message is not.

`shell_reveal_folder` checks the path is inside the user's own library before
opening it. Upstream's equivalent is `(_e, dir) => shell.openPath(dir)`, which
opens whatever the frontend names.

---

## Running it

```bash
npm install
npm run dev
```

Requires Rust (MSVC toolchain), Visual Studio Build Tools with the C++
workload, Node 22.18+, and WebView2 (present on Windows 11).

```bash
npm run ci          # frontend: lint, typecheck, reference tests
npm run vectors     # regenerate spec/ from the reference
npm run bench       # PE benchmark against an upstream checkout
npm run app:build   # release installer
```

```bash
cd src-tauri && cargo test --workspace
```

`--workspace` is not optional: without it cargo tests only the root package and
silently skips the core crate, which is where the vectors live.

The reference implementation's tests run directly against the TypeScript
sources through Node's native type stripping, so there is no build step for
them. That imposes one constraint on `src/`: **no TypeScript syntax that needs
code generation** — no `enum`, no `namespace`, no constructor parameter
properties, no `import =`. ESLint enforces it rather than leaving it to be
rediscovered at runtime.

---

## Installing, and putting it back

The part that writes. Split so that the code which *decides* cannot write, and
the code which writes makes no decisions.

**The plan is a pure function.** Every path, version transition and warning a
user sees before they agree is derived from data, with no filesystem access,
and the real run is handed that same structure. So the dry run and the install
cannot disagree about what is about to happen. Upstream decided as it copied,
which meant the only way to find out what an install would do was to let it
happen, and a refusal half-way left the folder in a state nobody had described.

Three warnings worth having, all of them shapes found on a real drive:

| Warning | Why it matters |
| --- | --- |
| `replacesUnmanagedFile` | The file being overwritten is not one we installed. It may be the game's own, or a swap the user did by hand and has forgotten. |
| `addsKindNotPresent` | Dropping frame generation into a game that never shipped it is a bigger change than swapping an upscaler. |
| `mixedVersionsAfterInstall` | One DLL upgraded and its sibling left at the old version. This is how a swap turns into a crash on launch. |

Version cohorts are compared **per kind**, because DLSS `310.8.0.0` beside
Streamline `2.13.0.0` is correct — comparing across kinds would fire that last
warning on every healthy install, which is worse than not having it.

**Preflight runs every check, always**, even after one has already failed: game
directory, store-protected location, path safety against the filesystem,
writability by actually writing, files held open by a running game, free space
on both the game and backup volumes, and the package's own contents. A user who
is told "the game is running", fixes it, and is then told "not enough space" has
been made to discover the situation twice.

Titles under `WindowsApps` are **refused with an explanation** rather than
attempted with elevation. Writing there means taking ownership of a system
directory, which breaks the store's own repair path. `C:\XboxGames`, the newer
layout, is an ordinary folder and is not caught by this.

**The journal makes an interruption recoverable.** NTFS offers no way to
replace several files as one transaction — transactional NTFS existed and
Microsoft deprecated it — so an install touching four DLLs genuinely has three
instants at which a power cut leaves a folder half-changed. The intent is
fsynced before the first target file is opened, and each completed step is
appended and flushed, so a later run can always answer what was happening and
what should happen now.

A half-applied install **rolls back rather than rolling forward**. Finishing
would need the source package to still exist and still be the same package, and
neither can be assumed after an unclean shutdown; rolling back needs only the
backups sitting beside the journal, so it is the option that is always
available. An install that fails rolls back through exactly the same code path
that recovery-after-a-crash uses, so the two cannot drift apart. The result
says how far it got — `nothingWritten`, `rolledBack` or `partiallyApplied` —
because the only question a user actually has is what state their folder is in.

**Backups outlive the journal.** The journal is bookkeeping and is deleted the
moment an install commits; the displaced originals are kept permanently in a
separate store, with the manifest pointing at them. An install from months ago
is still reversible.

**The manifest turns provenance into a fact.** The scanner's version-cohort
heuristic works, but it is an inference; a manifest records that we wrote this
file, at this version, with this hash. It also answers "is what I installed
still there?" — a real question, because a game patch overwrites the very files
a swap targets, which is why an upscaler swap silently reverts after an update.
Verification names the files rather than shrugging.

**Restore has no journal, deliberately.** Every step of it is an atomic replace
or a delete, each independently correct, and the manifest is cleared only once
all of them have finished — so an interrupted restore is repaired by running it
again. Two judgements a naive restore gets wrong, both of which would damage a
game folder:

- A **missing backup** means our file stays. Deleting it would leave the game
  with no runtime at all, which is worse than leaving it with a working one it
  did not ship.
- A file that is **no longer the one we installed** is left alone. The game has
  moved on, and writing a months-old backup over a freshly patched file would
  be the restore causing the damage it exists to undo.

---

## What exists, and what does not

Ported to Rust and passing the vectors — 206 Rust tests, 111 reference tests:

- Path safety, including the symlink walk and the cross-platform rules.
- Durable atomic writes with the Windows transient-replace retry.
- The hardened ZIP extractor.
- The settings schema, sanitiser, migration from upstream's `library.json`
  layout, and the store.
- PE inspection: the reader, its summary layer, and a persisted cache keyed
  on `(path, size, mtime)`.
- The job locks and cancellable parallel sweeps. A redesign rather than a
  translation: real threads instead of a promise chain, refuse-if-busy instead
  of queueing, and a test that asserts the pool actually runs in parallel.
- The folder scanner: API detection from imports with markers as a fallback,
  candidate ranking, skip lists, runtime-file provenance.
- Library discovery for Steam, Epic and Xbox, including a KeyValues reader.
- The validated command boundary and the app shell.
- The install engine: plan derivation, preflight, the write-ahead journal and
  its recovery, content hashing, the install manifest with integrity
  verification, and restore.

Two examples point the code at a real machine and print what it found:

```bash
cargo run -p neuralswap-core --example list_library
cargo run -p neuralswap-core --example scan_dir -- "<folder>"
```

The TypeScript under `src/core` and `src/main` is now purely the reference
implementation that `spec/` is generated from. Nothing in it ships.

### What building it twice caught

The case for the two-implementation approach is not theoretical. These are the
defects it found, each within seconds or minutes of the two being compared:

- The byte-limiting reader in the extractor checked its budget *before* each
  read but handed the full buffer to the inner reader, so a **stored** entry
  over-read past its own data and came out too long. Deflated entries hid it,
  because the decoder stops at the end of its stream. `std::io::Read::take`
  does this correctly and there was no reason to hand-roll it.
- `is_inside` sliced without a length check, which would panic rather than
  return `false`.
- `parseInt('8abc')` returns `8` in JavaScript, so the reference accepted
  `310.8abc` as `310.8` while a strict parse refused it. Strict is right, and a
  vector row now holds both implementations to it.
- `errors.ts` declared `ipcBadRequest` where Rust emits `badRequest` — in the
  file whose stated purpose is being the contract between them.
- Epic's `bIsIncompleteInstall` under `rename_all = "PascalCase"` becomes
  `BIsIncompleteInstall`, which never matched, so every half-downloaded title
  would have listed as installed.

Two more came from running the code against this machine rather than against
fixtures: the PE cache was write-only, so a rescan cost as much as a first scan
(the scanner took `&mut PeCache` and could not share it with its own threads),
and a hand-rolled substring search took six seconds on one 58 MB executable
where `memchr::memmem` takes milliseconds.

### Not built at all

- **The other two routes.** The engine installs a set of files into a folder;
  what it does not have is the knowledge of *which* files for ReShade + Feeder
  and for OptiScaler, both of which need more than dropping DLLs beside an
  executable. The native DLL swap is the route that works today.
- **Any notion of where packages come from.** No catalogue, no download, no
  signature check on what is fetched — you point it at a folder of runtime
  DLLs you already have, and it reads, hashes and classifies what is there.
- **Progress reporting during an install.** The window waits while it works.
  Fine for a handful of DLLs; not fine for a route that unpacks an archive.
- GOG discovery.
- The remaining trust features: GPU, driver and conflicting-injector checks in
  preflight, and a redacted diagnostics bundle.
- Localisation. Upstream ships 38 languages in two hand-maintained
  1,300-line JavaScript objects with no key-coverage check.
- Code signing and auto-update.

### On antivirus

Worth stating plainly, because it is the reason this project is Tauri and not
Electron — and because leaving Electron is *not* the fix.

Windows Defender quarantined this machine's freshly-downloaded `electron.exe`
during development. It had also quarantined upstream's shipped
`DLSS 5 Swapper.exe` the day before, under the **same threat ID**. Both are
unsigned; the detections were `Wacatac.F!ml`, `Bearfoos.AE!ml` and `Cinjo.O!cl`
— machine-learning and cloud heuristics, not signature matches. The variable
that was zero in both cases is the code signature, not the GUI framework.

So: signing is mandatory regardless of stack, and some flagging is irreducible
because of what the application *does* — it writes proxy DLLs next to game
executables. A single small signed binary accrues SmartScreen reputation faster
than a 150 MB bundle of unsigned files, which is a real secondary benefit of
this shape, but it is not a substitute. The answer to a user hitting this is
never "disable your antivirus".

The vector fixtures under `spec/` carry `.bin` extensions rather than `.exe`
and `.zip` for the same reason: a repository full of small executables
containing Direct3D entry-point strings, next to archives full of traversal
entries, is a quarantine waiting to happen on every clone.

## Licence

MIT.

[upstream]: https://github.com/rakanki911/DLSS5-Swapper
[advisory]: https://github.com/advisories/GHSA-jmr9-qjv8-65gv
