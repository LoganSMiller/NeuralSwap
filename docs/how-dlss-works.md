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

### 3.1 Frame generation is the exception: it cannot be fed at all

Frame generation has its own 70 KB guide, and it asks for considerably more
than the core guide implies:

- `kBufferTypeHUDLessColor` **and** the UI as its own layer
  (`kBufferTypeUIColorAndAlpha` or `kBufferTypeUIAlpha` — if both are tagged it
  prefers `UIAlpha`)
- `kBufferTypeBackbuffer`, tagged so its sub-rectangle is known
- Optionally `kBufferTypeBidirectionalDistortionField`
- And decisively:

> **It is required** for sl.reflex to be integrated in the host application.
> **Please note that any existing regular Reflex SDK integration (not using
> Streamline) cannot be used by DLSS-G.**

Reflex is not a buffer. It is a protocol the game takes part in, emitting
`eReflexMarkerPresentStart` and `eReflexMarkerPresentEnd` carrying frame indices
that must match the ones given to `slSetConstants`. Nothing outside the renderer
can take part on the game's behalf.

So frame generation is genuinely **out of reach** in a game that lacks it — not
degraded, not approximate. Every other feature can run on estimates of varying
quality; this one cannot run at all. An earlier version of our capability model
said "estimated" here, which was promising something impossible.

## 3.2 Two ways round the input problem that do not involve feeding

**The NVIDIA App does it at the driver level.** Its DLSS Override can *"update
hundreds of games to use the latest DLSS features including Multi Frame
Generation, and the newest AI models for DLSS Super Resolution, Frame
Generation, and Ray Reconstruction"* — **without modifying game files**, on
RTX 40 and 50 series.

For any game it covers, that is strictly better than what this application does:
no injection, no files written, no proxy DLL, NVIDIA-signed, and it survives
game updates. A tool that says "you do not need me for this, do it there" is
worth more than one that swaps files anyway. It is also the answer to wanting
lower overhead — there is no overhead at all.

The corollary is a hazard. **A driver-level override can mask a file swap.** If
the App is overriding the model for a game, replacing the DLL beside the
executable may produce no visible change, and the user will conclude our install
failed. Together with OTA (§5) that gives two independent reasons a
correctly-installed, correctly-verified swap can appear to do nothing.

**RTX Remix rebuilds the renderer instead of intercepting it.** For DirectX 8
and 9 games it *"takes the game's drawing instructions and renders everything
using real-time path tracing"*, with a bridge (`NvRemixBridge.exe`) to run
32-bit games in a 64-bit process.

That is why it escapes the constraint in §3: it is not recovering albedo from a
finished frame, it is doing the shading itself and therefore producing a genuine
G-buffer. The docs do not state that explicitly, so treat it as inference — but
it follows from path-tracing the scene.

The catch is the scope. Fixed-function DirectX 8/9 only, because those pipelines
expose enough semantic information to reconstruct a scene; a modern shader-based
renderer does not. It is a fourth route rather than a general answer, and it
needs per-game asset work rather than a file copy.

## 3.3 The driver already supplies some of this, locally

Worth knowing before building any sourcing feature. On this machine, under
`C:\Windows\System32\DriverStore\FileRepository\nvhmi.inf_amd64_*\`:

| File | Size | What it is |
| --- | --- | --- |
| `nvngx.dll` | 489 kB | the NGX loader that finds and dispatches to feature runtimes |
| `_nvngx.dll` | 1.4 MB | its companion |
| `nvngx_dlssg.dll` | **9.3 MB** | the **frame generation runtime**, driver-supplied |
| `nvngx_update.exe` + `nvidia-ngx-updater` | 1.0 MB + 6.2 MB | the OTA mechanism from §5, on disk |

So the driver ships the frame generation runtime and the NGX loader outright,
and carries its own updater. Two consequences:

- **The user's own driver install is a legitimate local source.** For source
  discovery, the DriverStore should be searched alongside their games — no
  download, no mirror, no redistribution, and the file is by definition a
  genuine NVIDIA build. This directory covers one feature of four.
  **But see §3.4 — that is a fact about this directory, not about the driver.**
- **`nvngx_update.exe` is the OTA machinery**, which makes §5's caveat concrete
  rather than theoretical: there is a program on disk whose job is replacing
  these files.

Also installed: **FrameView SDK** (`FvSDK_x64.dll` plus headers and a service).
That is NVIDIA's frame-timing measurement library, and it is the obvious way to
answer "what did this install actually do to my frame times" with a measurement
rather than a claim. It has its own licence and needs its service running, so it
is an opportunity noted rather than a dependency taken.

## 3.4 The NGX model store carries three features of four

The DriverStore is not where the driver keeps the runtimes it actually serves.
That is `C:\ProgramData\NVIDIA\NGX\models`, and it is entirely self-describing:

```
models/
  nvngx_config.txt                                which version is active, per component
  dlss/versions/20318081/files/160_E658700.bin    74 MB  → nvngx_dlss.dll
  dlssd/versions/20318081/files/160_E658700.bin   80 MB  → nvngx_dlssd.dll
  dlssg/versions/20318081/files/160_E658700.bin    7 MB  → nvngx_dlssg.dll
  dlss_override/versions/20318081/files/160_E658700/
      nvngx_package_config.txt                    declares the three above
      sl.dlss.dll  sl.dlss_d.dll  sl.dlss_g.dll  sl.common.dll  …
```

Each `nvngx_package_config.txt` is one comma-separated row per file —
`component, version, stored extension, real name`:

```
dlss, 310.7.129, .bin, nvngx_dlss.dll
sl_common_0, 2.14.0, .dll, sl.common.dll
```

So the store declares both what a file *is* and what it must be called when
installed. Version keys are shared across components, which is how a matched
set stays matched. Runtimes are stored as `.bin`; they are ordinary PE images.

**Three of four features are sourceable from the driver**, at 310.7.129 —
super resolution, ray reconstruction and frame generation — plus complete
Streamline sets at 2.12.129 and **2.14.0**, which is newer than any public SDK.
Measured on this machine, source discovery finds 3 driver candidates against
19 from 11 installed games, and it is the driver's copy that ranks first
because its provenance needs no inference.

**Neural rendering is the exception, and its absence is informative.** There is
no `nvngx_dlssnr.dll` and no `sl.dlss_nr.dll` anywhere in the store. The driver
does not carry neural rendering, so it cannot be sourced this way — on this
machine the only copy is the one a tool hand-installed into a game.

### The declared version beats the file's own

These runtimes are tens to hundreds of megabytes of model weights around a thin
PE wrapper. Scanning one for a `VS_FIXEDFILEINFO` signature finds a match
*inside the weights* long before the real resource, and reads out as
`46863.0.46863.4696` for every file in the store. The manifest says
`310.7.129`. Where the two disagree the manifest wins: it is a statement by the
installer, not a guess about a byte pattern.

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

## 5.1 The plugin manifests are the ground truth

Every Streamline plugin embeds a JSON manifest naming its feature id, the
graphics APIs it supports, and the plugins it depends on. These are readable
straight out of the shipping DLLs, and they settle several questions that the
public SDK headers get asked to answer and cannot.

| plugin | id | rhi | requires |
|---|---|---|---|
| `sl.common` | -1 | d3d11, d3d12, vk | — |
| `sl.dlss` | 0 | d3d11, d3d12, vk | `sl.common` |
| `sl.nis` | 2 | d3d11, d3d12, vk | `sl.common` |
| `sl.reflex` | 3 | d3d11, d3d12, vk | `sl.common` |
| `sl.pcl` | 4 | d3d11, d3d12, vk | `sl.common` |
| `sl.dlss_g` | 1000 | **d3d12, vk** | `sl.common`, **`sl.reflex`** |
| `sl.dlss_d` | 1001 | d3d11, d3d12, vk | `sl.common` |
| `sl.dlss_nr` | **1004** | **d3d12, vk** | `sl.common` |

Four things follow.

**Neural rendering is a real Streamline feature.** It is id 1004, shipped as
`sl.dlss_nr` from 2.13. The public SDK on GitHub is **2.12**, which has no
neural-rendering entry anywhere — no feature id, no `sl_dlss_nr.h`. Reading
only the public SDK leads to the conclusion that no game can ask for neural
rendering at all. That conclusion is wrong, and this project held it until the
manifests were read. A game built against 2.13 requests it like anything else.

**But almost no game ships it yet.** Cyberpunk 2077, a flagship DLSS title,
ships `sl.dlss`, `sl.dlss_d` and `sl.dlss_g` beside its executable and *not*
`sl.dlss_nr`. So it feeds everything neural rendering consumes while never
asking for it. That is a fact about the game, not the feature — which is why
the check belongs on the file list rather than in a hard-coded rule.

**Frame generation's dependency on Reflex is structural.** `sl.dlss_g` lists
`sl.reflex` in `required_plugins`. This is the binary confirming what the
programming guide says in prose, and it is why frame generation is out of reach
on any route that cannot participate in the Reflex protocol.

**Only frame generation and neural rendering refuse D3D11.** Ray reconstruction
accepts `d3d11` — reasoning by analogy from the other 1000-series features says
otherwise, and is wrong. This is the constraint the bridge route exists for.

The manifests are extracted with a search for `"namespace"` in the DLL, then a
scan back to the enclosing `{`. They are plain UTF-8, uncompressed.

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

### 6.1 The dumpbin check is scoped, and reading it as universal is a bug

The quoted correctness check above is introduced by the guide with "if you are
integrating Streamline by replacing the standard libraries with
`sl.interposer.lib`". It validates *one* of the two supported integration
styles. The other is **manual hooking**, and the guide is explicit:

> keep linking the standard libraries, load `sl.interposer.dll` dynamically and
> redirect DXGI/D3D API calls as required

> If you are using Vulkan, instead of `vulkan-1.dll` dynamically load
> `sl.interposer.dll`

A game integrated that way imports `d3d12.dll` or `vulkan-1.dll`, never names
anything `sl.*`, and has a complete Streamline integration. On imports alone it
is indistinguishable from a game with no plumbing at all — and it would be
routed to an expensive bridge that manufactures inputs it already produces. For
Vulkan this is the style NVIDIA *recommends*, so it is not a corner case.

What settles it is that `sl.interposer.dll` and `sl.common.dll` are mandatory
redistributables that "need to be distributed with your application", installed
next to the host executable. Whatever the linkage, the interposer is on disk.

So the rule is asymmetric, and deliberately so:

- **Imports lead** for DLSS runtimes. `nvngx_*.dll` files get left behind by old
  game versions and copied in by hand, so presence proves nothing about use.
- **Disk is decisive for Streamline.** Nothing but an integration puts
  `sl.interposer.dll` in a game folder.

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

---

## 10. What the community stack actually requires

Read from the seven tools' own sources rather than inferred. This is the part
that decides whether an install works, and almost all of it fails *silently*
when it is wrong.

### The add-ons are three different things with similar names

| Add-on | What it is | Where from |
| --- | --- | --- |
| ShortFuse's **`renodx-dlss`** | Does the whole job alone on 64-bit DX9/11/12. **Replaces** Feeder rather than working with it. | RenoDX Discord |
| Krish's **`renodx-dlss5.addon64`** | A neural *consumer*: answers the request Feeder builds. | RenoDX Discord, `#DLSS5` |
| **Deep Fried Chicken** | The recommended consumer. Three files plus a config. | its author's Discord |

Feeder builds the DLSS request a game never makes; a consumer performs the
neural rendering. Neither consumer is published anywhere fetchable, and neither
bundles `nvngx_dlssnr.dll` — the user supplies that, which is what our source
discovery automates. Feeder's own instructions say to take `nvngx_dlss.dll`
"from any DLSS game", which is precisely the search we do with provenance
attached.

### Seven ways an install silently does nothing

1. **Two neural consumers installed.** The first "does nothing at all for the
   whole session — silently". Feeder's README names this as the first thing to
   check.
2. **Feeder and the DX11 bridge both installed** for one game. They are
   alternatives: the bridge is for games that *have* DLSS.
3. **OptiScaler left enabled** on the Feeder route.
4. **The motion-vector shader compiled for one provider while another is
   enabled** — called "the classic silent failure" outright.
5. **DRME as the provider.** It does not compile on ReShade 6.8
   (`X3020: cannot sample from texture that is also used as render target`),
   still appears enabled, and writes nothing — so DLSS runs with no motion
   vectors at all.
6. **The provider's technique below `DLSS 5 Feed`** in the load order.
7. **ReShade 6.8's own installer writes a malformed search path** —
   `Shaders\**\**`, a double glob its resolver cannot canonicalize (Win32
   rejects wildcards, error 123), so it skips the path and no effects are ever
   found. FeedKit collapses it to `Shaders\**`. Note also that ReShade's INI
   parser is case-sensitive, so a wrong-case key is silently dead.

Every one of these is checkable before writing anything, which is the
opportunity: the existing tools detect some of them at runtime and log them.

### Other conflicts worth knowing

NVIDIA **Smooth Motion** must be off for Vulkan, settable per-application
through driver profile IDs `0xB0CC0875` (enabled-APIs bitfield: 1 DX12, 2 DX11,
4 Vulkan), `0xB0D384C0` (enable) and `0xB01B8B02` (debug bars, which draw
coloured bars on generated frames — a genuinely useful diagnostic). The game's
own MSAA/SSAA must be off.

### The 32-bit path, and why it exists

NVIDIA ships **no 32-bit NGX runtime at all**, so an in-process approach is
impossible by construction. Feeder runs the 64-bit stack in a separate
`host64\` helper process with its own ReShade install and its own `ReShade.ini`.
That is why `dlss5-feed-host64.exe` exists, and why a 32-bit install configures
ReShade twice.

### Corroborated pins

Two independent projects — upstream DLSS5-Swapper and the guide launcher — pin
`ReShade.fxh` at the same digest, `6dabfbba…`, from crosire's shader repository.
An independently corroborated hash is a better pin than one taken on trust from
a single source.

### "SF" decoded

RHI's `dlssnr-310.8.SF` and `-SF-v2` are the patched neural rendering runtimes;
the AIO installer's file index names the same thing "RenoDX SF patched",
covering RTX 20/30/40/50. Worth recording only so the naming is not a mystery
when it turns up in a folder — NeuralSwap neither ships nor routes them.
