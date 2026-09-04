# How DLSS actually works, and what follows for NeuralSwap

Written from primary sources: NVIDIA's [Streamline SDK][sl] and its programming
guides, the [NGX programming guide][ngx], [ReShade][reshade]'s add-on API as
used by [DLSS5-Feeder][feeder], and [OptiScaler][opti]'s export surface.

This is not a summary of those documents. It records the parts that change what
this application must do, and the reasoning is here so a future decision can be
argued with rather than guessed at.

---

## 1. Three layers, not one

| Layer | Files | What it is |
| --- | --- | --- |
| **NGX** | `nvngx_dlss.dll`, `nvngx_dlssd.dll`, `nvngx_dlssg.dll`, `nvngx_dlssnr.dll` | The feature runtimes. The actual neural networks and the code that runs them. |
| **Streamline (SL)** | `sl.interposer.dll`, `sl.common.dll`, `sl.dlss.dll`, `sl.dlss_g.dll`, … | A vendor-neutral plumbing layer that sits between the game and the graphics API and brokers features. |
| **The game** | its own executable | Integrates *either* NGX directly *or* Streamline. |

The distinction matters constantly. "Swapping DLSS" can mean replacing an NGX
runtime, replacing the Streamline plugins, or both — and the two families are
versioned independently. DLSS `310.8.0.0` beside Streamline `2.13.0.0` is
correct and healthy, which is why the install planner compares version cohorts
**per kind** and would otherwise warn on every well-formed install.

---

## 2. The runtime is found beside the executable

The NGX guide is explicit: the feature DLLs "should be installed in the same
folder as your application's executable (or DLL if you are building a plugin)",
and during development you copy `nvngx_*.dll` next to the executable "so NGX
runtime can find DLLs."

Two consequences the code depends on:

- The install directory is **the folder containing the executable that loads the
  runtime**, not the game's root. This is why `install_dir` is derived from the
  scanner's chosen candidate rather than assumed, and why a game like Cyberpunk
  installs into `bin\x64\` rather than the top level.
- A copy anywhere else is inert. The scanner's `notBesideExecutable` provenance
  verdict is not a heuristic guess about tidiness — such a file genuinely
  cannot be loaded, and telling the user it will do nothing is correct.

`NVSDK_NGX_*_Init` also takes an **application data path** that needs write
access, used for logs and temporary files. That is where `dlss5-feed.log` and
similar diagnostics appear, and it is why a read-only game folder can break a
feature that would otherwise work.

---

## 3. What DLSS demands as input

Streamline features consume tagged resources. The buffer types, from
`sl_consts.h` by way of the programming guide:

| Tag | Id | Note |
| --- | --- | --- |
| `kBufferTypeDepth` | 0 | must be usable with the `clipToPrevClip` transform |
| `kBufferTypeMotionVectors` | 1 | object, optionally camera |
| `kBufferTypeHUDLessColor` | 2 | post-processing applied, no UI |
| `kBufferTypeScalingInputColor` | 3 | **jittered** input |
| `kBufferTypeScalingOutputColor` | 4 | the result |
| `kBufferTypeNormals`, `Roughness`, `Albedo`, `SpecularAlbedo`, `IndirectAlbedo` | 5–9 | G-buffer, for ray reconstruction and neural rendering |

On top of the tags, **per-frame camera constants are required by every SL
feature** and must be supplied "as early in the frame as possible."

This is the single most clarifying fact in the whole stack, because it explains
the entire route structure:

- A game **with** DLSS already produces all of this. Swapping its runtime is
  therefore just a file operation — the contract is already being satisfied.
- A game **without** DLSS produces none of it. Something must manufacture a
  plausible depth buffer, motion vectors and jitter from what is available.
  That is exactly what Feeder does, building the contract from ReShade's depth
  buffer plus computed motion vectors.
- Motion vectors are the hard part. A game that never had DLSS has no reason to
  expose them, so they have to be **derived by optical flow** — which is why the
  Feeder route depends on a motion-estimation shader pack (LumeniteFX, VORT, or
  JakobPCoder's) and does not work without one.
- Neural rendering needs more than upscaling does: the G-buffer tags above.
  That is why NR is harder to feed than super resolution, and why the DX11
  bridge route exists at all — mirroring a game's real DLSS onto a private D3D12
  session yields a genuine contract instead of a synthesised one.

---

## 4. Signing is part of the loading contract

Every module in Streamline's `bin/x64` is **dual-signed** by NVIDIA: a standard
Windows certificate verifiable with `WinVerifyTrust`, plus a custom NVIDIA
certificate for the case where the OS certificate store cannot be trusted.

The important part for a swapping tool:

> The prebuilt binary automatically performs the above steps when loading SL
> plugins to ensure maximum security.

So `sl.interposer.dll` **verifies the plugins it loads**. A swap that puts an
unsigned or mismatched module beside a signed interposer does not fail loudly at
install time — it fails at feature initialisation, inside the game. This is the
mechanism behind the `HashMismatch` / permanent `STANDBY`/`FAILED` reports with
status `0xBAD00002` that appear in the issue trackers of tools in this space.

NVIDIA's own guidance is that unsigned development builds require the
application's signature checking to be temporarily disabled, and "should be used
ONLY in development/debugging situations and never shipped."

Consequences for us:

- A mixed set — some modules replaced, some original — is not merely untidy, it
  is a *likely failure*. The `mixedVersionsAfterInstall` warning is describing a
  real breakage mode, not a cosmetic one.
- A **patched** runtime has lost its signature by definition. Any tool
  distributing one is distributing something the interposer is designed to
  refuse, and the failure surfaces to the user as an unexplained
  `0xBAD00002`. This is one of several reasons NeuralSwap does not ship or
  route patched binaries.
- Reading Authenticode from a candidate DLL would let us say "this is a genuine
  NVIDIA build" versus "this is something else" *before* writing it. That is
  worth building and is not yet built.

---

## 5. What is on disk is not always what runs

Streamline supports **over-the-air updates**, opted *in* by default: when
enabled, "SL will look for the latest SL/NGX updates and load newer versions of
required feature(s) if available."

So a game may load a runtime newer than the one sitting beside its executable.
This bounds what our integrity verification can honestly claim: `FileStatus`
answers "is the file we wrote still the file we wrote", which is a real and
useful question, but it is *not* the same as "is this the version the game is
using." Where a swap appears to have no effect and the files verify as intact,
an OTA override is a leading explanation — as is NVIDIA App's profile-level
override, which RTX40MFG-Unlock's README also notes can supply the frame
generation wrapper separately.

---

## 6. Detecting which route a game needs

Streamline is integrated by **replacing** the graphics libraries: an integrated
application links `sl.interposer.dll` *instead of* `dxgi.dll`, `d3d11.dll`,
`d3d12.dll` or `vulkan-1.dll`. NVIDIA's own correctness check is a dependency
scan:

> Run `dumpbin /dependents` … Look for `dxgi.dll`, `d3d11.dll`, `d3d12.dll` or
> `vulkan-1.dll` in the list of dependents. If you see either of those or if
> `sl.interposer.dll` is missing in the list then SL was **NOT** integrated
> correctly.

We already read import tables. That makes route selection a matter of evidence
rather than inference:

| Imports | Meaning | Route |
| --- | --- | --- |
| `sl.interposer.dll` | Streamline-integrated | native swap — the contract already exists |
| `nvngx*` | NGX used directly | native swap, NGX layer only |
| `d3d12.dll` / `dxgi.dll` only | no DLSS plumbing | bridge (if the game has its own DLSS) or Feeder |
| `d3d11.dll`, `vulkan-1.dll` | ditto, and DX11/Vulkan | bridge or Feeder |

This is strictly better than guessing from the presence of `nvngx_*.dll` files
in the folder, because a file can be present and unused, and because the import
table describes what the executable will actually load.

**With one honest limit.** Many engines — Unity especially — resolve Direct3D
through `LoadLibrary` at startup, so their import table names no graphics API at
all. Measured on the development machine's eleven installed games:

| Game | Imports | Verdict |
| --- | --- | --- |
| Cyberpunk 2077 | `sl.interposer.dll` **only** | Streamline → native swap |
| Ready or Not, Escape the Backrooms (Unreal) | `dxgi`, `d3d11`, `d3d12`, `d3d9`, `opengl32` | no plumbing, DLSS files present → native swap, then Feeder |
| 9 Kings, Gambonanza, Slay the Spire 2 (Unity) | *nothing* | undetermined → Feeder |

Cyberpunk imports the interposer and *nothing else*, which is precisely the
shape NVIDIA's guide describes for a correct integration. Unreal titles import
the APIs directly and load DLSS via their own plugin at runtime, so the files
are there without a static import.

The empty case is reported as `Undetermined`, not as `None`. An absent import is
not evidence of an absent feature, and the same rule applies here as everywhere
else in this codebase: uncertainty is stated rather than rounded to a confident
answer. The route is the same either way — Feeder works regardless — but the
sentence shown to the user says "we cannot tell from the file" instead of
asserting the game has no DLSS.

---

## 7. ReShade is the injection host

ReShade replaces a graphics DLL the game already loads (`dxgi.dll`,
`d3d11.dll`, `winmm.dll`, `version.dll`, and so on — the classic proxy-DLL
approach), then exposes an **add-on API** to code that wants to observe or
modify the frame.

An add-on is an ordinary DLL that calls `reshade::register_addon(module)` from
`DllMain` and then registers callbacks. Feeder uses:

`create_device`, `destroy_device`, `init_effect_runtime`,
`destroy_effect_runtime`, `reshade_reloaded_effects`,
`reshade_render_technique`, `reshade_present`, `reshade_overlay`,
`reshade_open_overlay`, `set_fullscreen_state`.

Two practical notes worth keeping:

- **Only an "Addon" build of ReShade can load add-ons.** The vanilla build
  cannot, so an install that fetches the wrong variant fails in a way that looks
  like the add-on is broken. Our catalogue's ReShade entry points at the
  `_Addon` installer for this reason.
- Feeder's source carries a comment that its work cannot happen in `DllMain`
  because it calls `LoadLibrary`, which can deadlock under the loader lock. Any
  add-on we ever write has the same constraint.

ReShade also registers itself as a **Vulkan layer** rather than proxying a DLL
when the target is Vulkan, via the registry plus a JSON manifest whose
`layer.library_path` points at the ReShade DLL. That is a different install
shape from copying files beside an executable, and a route that supports Vulkan
has to implement it.

---

## 8. OptiScaler is a proxy that translates

OptiScaler (GPL-3.0, ~10k stars) exports the full surface of `d3d12`, `dxgi`,
`dbghelp`, `version`, `winhttp`, `wininet` and `winmm`. It is renamed to
whichever of those the target game loads, forwards the genuine calls onward, and
intercepts the upscaler API in between — so it can present DLSS inputs to a game
that only ever asked for FSR or XeSS, or the reverse.

The consequence for install planning is that OptiScaler's filename **is** its
configuration: which name it takes decides whether it loads at all, and it must
not collide with a name ReShade has already claimed. Upstream picks `winmm.dll`
for Vulkan targets and `dxgi.dll` otherwise, precisely because ReShade normally
owns `dxgi.dll` for DirectX.

---

## 9. What this means for NeuralSwap

Decisions that follow directly from the above, recorded so they are not
relitigated from memory:

1. **Install beside the executable that loads the runtime.** Not the game root.
   Already implemented; §2 is why.
2. **Compare version cohorts per kind.** NGX and SL version independently.
   Already implemented; §1 is why.
3. **Treat a mixed set as a probable failure, not untidiness.** §4.
4. **Select the route from the import table.** `sl.interposer.dll` is decisive
   evidence of Streamline integration. Worth implementing — see §6.
5. **Verify Authenticode before writing an NVIDIA DLL**, so "genuine NVIDIA
   build" is a fact we can state. Not yet implemented; §4.
6. **Do not claim integrity verification proves what the game is running.** OTA
   and driver-level overrides both break that inference. §5.
7. **The Feeder route requires a motion-estimation shader pack.** It is a hard
   dependency, not a nicety, and the catalogue should express it. §3.
8. **A Vulkan route needs layer registration**, not file copying. §7.
9. **OptiScaler's proxy name is part of its plan** and can collide with
   ReShade. §8.

[sl]: https://github.com/NVIDIA-RTX/Streamline
[ngx]: https://docs.nvidia.com/ngx/programming-guide/index.html
[reshade]: https://reshade.me
[feeder]: https://github.com/jlrouzies-fr/DLSS5-Feeder
[opti]: https://github.com/optiscaler/OptiScaler
