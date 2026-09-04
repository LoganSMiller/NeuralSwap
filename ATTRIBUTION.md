# Attribution

## The project this one learns from

[rakanki911/DLSS5-Swapper](https://github.com/rakanki911/DLSS5-Swapper), MIT.

NeuralSwap is a separate implementation, not a fork: no upstream source is
copied or adapted, and every file here was written from scratch. What it takes
from that project is knowledge, which is worth naming precisely because it is
the expensive part:

- **The problem is worth solving at all**, and the three-route shape of the
  solution — swap the native runtime where a game already has one, drive
  ReShade plus a feeder where it does not, or hand off to OptiScaler.
- **A write-ahead file-level journal is the right primitive.** Upstream's
  `src/core/file-journal.js` snapshots individual files before touching them
  and can roll an interrupted install back exactly, rather than
  snapshotting or recursively deleting a whole game folder. That design
  decision is correct and this project will follow it.
- **Path validation must refuse symlinks, not just check prefixes.** Upstream's
  `safePath` walks every path component looking for reparse points instead of
  trusting `startsWith`. This project's validator starts from that idea and
  extends it to DOS device names, alternate data streams and trailing-dot
  ambiguity.
- **One route policy shared by the UI and the installer.** Upstream's
  `install-routes.js` is imported by both, so the sheet cannot offer an option
  the installer will refuse.
- **PE markers, not just the import table.** A game that reaches Direct3D
  through `LoadLibrary` has no import entry for it, but the entry-point name is
  still in the binary as a string. Upstream's detection handles this, and its
  test corpus documents the specific titles where it matters.
- **Pin a hash for every download**, and treat the anti-cheat question as a
  risk the user acknowledges rather than something to work around.

Where this project disagrees with upstream, the reasoning is in the code
comments and in the README, next to the measurement that prompted it.

## Naming

Upstream is called "DLSS 5 Swapper". DLSS is NVIDIA's trademark, and a
third-party tool that injects modified runtimes into games is not well served
by putting someone else's mark in its product name. "NeuralSwap" is deliberate
prudence, not a branding preference.

This project is not affiliated with, endorsed by, or connected to NVIDIA
Corporation.

## Third-party components

None are bundled here yet. When the install routes are built, these are the
components they orchestrate; each is downloaded from its own upstream release,
verified against a pinned hash, and governed by its own licence:

| Component | Purpose | Licence |
| --- | --- | --- |
| [ReShade](https://reshade.me) (crosire) | Add-on host and injector | BSD-3-Clause / see project |
| [DLSS5-Feeder](https://github.com/jlrouzies-fr/DLSS5-Feeder) | Synthesises a DLAA contract for games with no DLSS | MIT |
| [OptiScaler DLSS-NR](https://github.com/Dagherbou/OptiScaler_DLSSNR) | Alternative neural-rendering route | GPL-3.0 |
| [RenoDX](https://github.com/clshortfuse/renodx) | ReShade add-on | MIT |
| [dgVoodoo2](https://github.com/dege-diosg/dgVoodoo2) | DirectX 8/9 to DirectX 11 translation | Freeware, see project |
| [vort_Shaders](https://github.com/vortigern11/vort_Shaders) | Motion-vector shaders | MIT |
| [reshade-shaders](https://github.com/crosire/reshade-shaders) | Shader headers | See project |

NVIDIA's DLSS runtime files are NVIDIA's, redistributed by nobody here: the
native route swaps files already present on the user's machine or obtained from
NVIDIA's own distribution.

## Obligations

Every component above keeps its own licence, and several are copyleft.
`THIRD_PARTY_NOTICES.md` will be generated at build time from the pinned
manifest — rather than maintained by hand — so a component cannot be added
without its notice shipping alongside it.
