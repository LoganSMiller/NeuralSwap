//! Which DLSS features can actually work in a given game, and why not when
//! they cannot.
//!
//! The ambition is every feature in every game. The reason that is not simply
//! a matter of effort is that DLSS features consume *renderer-internal* data,
//! and some of it does not exist anywhere outside the renderer that produced
//! it. From the Streamline guides:
//!
//! - Every feature requires `cameraViewToClip`, `clipToPrevClip` and
//!   `prevClipToClip`, row-major, and explicitly **without** the temporal-AA
//!   jitter folded in - jitter is passed separately as `jitterOffset`.
//! - Ray reconstruction additionally requires `kBufferTypeAlbedo` and
//!   `kBufferTypeSpecularAlbedo` in a linear format, normals and roughness,
//!   and specular hit distance.
//!
//! Albedo cannot be recovered from a finished frame: the lighting has already
//! been applied to it. And a jittered input colour cannot be imposed from
//! outside, because jitter is a change to how the game samples - which is
//! precisely why the Feeder route describes what it supplies as a *synthetic
//! DLAA contract* rather than upscaling.
//!
//! So this module does not pretend. It reports, per feature, whether the
//! inputs are real, mirrored from the game's own DLSS, or estimated - and when
//! they are estimated it says which ones, because that is where the difference
//! between a game that looks excellent and one that looks wrong comes from.
//!
//! See `docs/how-dlss-works.md` §3 for the sourcing.
//!
//! # Two known rough edges
//!
//! Both were found by running this against real game folders, and neither is
//! fixed yet:
//!
//! 1. **Ray reconstruction and neural rendering are modelled with the same
//!    input set**, because neural rendering's requirements are not published
//!    and ray reconstruction is the closest documented analogue. A consequence
//!    is that a game feeding one is reported as feeding the other, so a title
//!    with `nvngx_dlssnr.dll` but no `nvngx_dlssd.dll` is told ray
//!    reconstruction is native when in fact its runtime is not even present.
//!    The fix is to require the feature's own runtime to exist before calling
//!    it native, and to distinguish "replace this" from "add this".
//!
//! 2. **Provenance cannot always tell a shipped runtime from an installed
//!    one.** [`Feature::fed_by_game`] filters on version cohort, which catches
//!    the common case, but a tool that installed a runtime matching its
//!    siblings' versions is indistinguishable by that test. An install
//!    manifest makes it a fact; short of that, an Authenticode check would
//!    at least separate a genuine NVIDIA build from something else.

use serde::{Deserialize, Serialize};

use crate::platform::gpu::Generation;
use crate::scan::integration::{Integration, Route};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Feature {
    /// Upscaling, or DLAA at native resolution.
    SuperResolution,
    /// Denoising for ray-traced rendering.
    RayReconstruction,
    /// Generated intermediate frames.
    FrameGeneration,
    /// DLSS 5 neural rendering.
    NeuralRendering,
}

impl Feature {
    pub const ALL: [Feature; 4] = [
        Feature::SuperResolution,
        Feature::RayReconstruction,
        Feature::FrameGeneration,
        Feature::NeuralRendering,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Feature::SuperResolution => "Super Resolution",
            Feature::RayReconstruction => "Ray Reconstruction",
            Feature::FrameGeneration => "Frame Generation",
            Feature::NeuralRendering => "Neural Rendering",
        }
    }

    /// The runtime file that implements it.
    pub const fn runtime(self) -> &'static str {
        match self {
            Feature::SuperResolution => "nvngx_dlss.dll",
            Feature::RayReconstruction => "nvngx_dlssd.dll",
            Feature::FrameGeneration => "nvngx_dlssg.dll",
            Feature::NeuralRendering => "nvngx_dlssnr.dll",
        }
    }

    /// The tagged resources it consumes, beyond the camera constants every
    /// feature needs.
    pub fn inputs(self) -> Vec<Input> {
        let base = [Input::Depth, Input::MotionVectors, Input::CameraMatrices];
        match self {
            Feature::SuperResolution => {
                let mut all = base.to_vec();
                all.extend([Input::JitteredColor, Input::JitterOffset]);
                all
            }
            // Frame generation's dedicated guide asks for considerably more
            // than the core guide implies: the UI as its *own* layer as well
            // as a HUD-less colour buffer, the backbuffer tagged for subrect
            // data, and - decisively - Reflex integrated through Streamline.
            Feature::FrameGeneration => {
                let mut all = base.to_vec();
                all.extend([
                    Input::HudLessColor,
                    Input::UiLayer,
                    Input::Backbuffer,
                    Input::ReflexMarkers,
                ]);
                all
            }
            // Ray reconstruction's requirements are documented; neural
            // rendering's are not public, and it is modelled on the same
            // G-buffer class because that is the closest documented analogue
            // and matches what the community routes actually supply.
            Feature::RayReconstruction | Feature::NeuralRendering => {
                let mut all = base.to_vec();
                all.extend([
                    Input::JitteredColor,
                    Input::Albedo,
                    Input::SpecularAlbedo,
                    Input::NormalsRoughness,
                ]);
                all
            }
        }
    }

    /// Whether this build is reasoning from documented requirements or from
    /// inference. Stated because the UI should not present the two alike.
    pub const fn requirements_documented(self) -> bool {
        !matches!(self, Feature::NeuralRendering)
    }

    /// The Streamline plugin that implements this feature, and its feature id.
    ///
    /// Read out of the plugins' own embedded JSON manifests, in the shipping
    /// Streamline 2.13 binaries rather than from a header:
    ///
    /// ```text
    /// plugin          id     rhi                    requires
    /// sl.common       -1     d3d11, d3d12, vk       -
    /// sl.dlss          0     d3d11, d3d12, vk       sl.common
    /// sl.nis           2     d3d11, d3d12, vk       sl.common
    /// sl.reflex        3     d3d11, d3d12, vk       sl.common
    /// sl.pcl           4     d3d11, d3d12, vk       sl.common
    /// sl.dlss_g     1000     d3d12, vk              sl.common, sl.reflex
    /// sl.dlss_d     1001     d3d11, d3d12, vk       sl.common
    /// sl.dlss_nr    1004     d3d12, vk              sl.common
    /// ```
    ///
    /// Every row was read from a shipping binary: `sl.dlss_d` from a game's
    /// own install, the rest from the 2.13 package. The two that matter are
    /// the two that are *narrower* than the others.
    ///
    /// # A correction
    ///
    /// An earlier version of this module claimed neural rendering was not
    /// something a game could integrate at all, on the grounds that the public
    /// SDK's feature enumeration has no entry for it. That enumeration is real,
    /// but the public SDK is **2.12**, and neural rendering arrived in 2.13 as
    /// `sl.dlss_nr`, feature id 1004. A game built against 2.13 can request it
    /// like any other feature.
    ///
    /// What remains true is the practical situation: 2.13 is new, so a game
    /// that ships it is currently the exception. That is a fact about a
    /// particular game, though, not a property of the feature - so it is
    /// answered by looking at which plugins the game actually ships, which is
    /// what [`Feature::fed_by_game`] does.
    pub const fn streamline_plugin(self) -> (&'static str, i32) {
        match self {
            Feature::SuperResolution => ("sl.dlss.dll", 0),
            Feature::RayReconstruction => ("sl.dlss_d.dll", 1001),
            Feature::FrameGeneration => ("sl.dlss_g.dll", 1000),
            Feature::NeuralRendering => ("sl.dlss_nr.dll", 1004),
        }
    }

    /// Whether the feature refuses to run on Direct3D 11.
    ///
    /// From the `rhi` list in each plugin manifest above. Only `sl.dlss_g` and
    /// `sl.dlss_nr` are restricted to `d3d12` and `vk`; everything else,
    /// including ray reconstruction, accepts `d3d11`. So on a DX11 game frame
    /// generation and neural rendering cannot be reached through Streamline
    /// however the files are arranged - which is the reason the bridge route
    /// exists, rather than a preference for it.
    ///
    /// Ray reconstruction was assumed to be restricted here too, by analogy,
    /// until its manifest was read. The manifests are the source; the analogy
    /// was wrong.
    pub const fn requires_modern_rhi(self) -> bool {
        matches!(self, Feature::FrameGeneration | Feature::NeuralRendering)
    }

    /// The oldest architecture that can run this feature at all.
    ///
    /// A second, independent gate: the input contract is about what the *game*
    /// provides, this is about what the *card* can execute. Both have to hold,
    /// and confusing them is how a user ends up told a feature is available
    /// when their hardware cannot run it.
    ///
    /// Sourced from NVIDIA's Omniverse RTX renderer documentation, which lists
    /// per-architecture support plainly: ray reconstruction on Turing, Ada and
    /// Blackwell but not on the Ampere compute cards, which have no RT cores;
    /// frame generation only on Ada and Blackwell, because it needs the optical
    /// flow accelerator those introduced.
    pub const fn minimum_generation(self) -> Generation {
        match self {
            // Tensor cores, so RTX 20-series onward. The GTX 16-series is
            // Turing silicon without them.
            Feature::SuperResolution => Generation::Turing,
            // Needs RT cores as well as tensor cores.
            Feature::RayReconstruction => Generation::Turing,
            // The optical flow accelerator arrived with Ada. This is the gate
            // the RTX 40 unlock exists to widen - it raises the *multiplier*
            // on Ada, it does not bring the feature to older cards.
            Feature::FrameGeneration => Generation::Ada,
            // Gated to Blackwell. Inferred rather than documented, like the
            // rest of neural rendering's requirements.
            Feature::NeuralRendering => Generation::Blackwell,
        }
    }

    /// Which feature a runtime filename implements.
    /// The feature a *runtime* file implements.
    ///
    /// Deliberately only `nvngx_*.dll`. This answers "installing this file
    /// provides that feature", so it has to stay narrow: a Streamline plugin
    /// is 400 KB of brokering and the runtime it brokers is 70 MB of model
    /// weights, and a moment of treating the two alike had the installer
    /// offering to write `sl.dlss.dll` into a game as `nvngx_dlss.dll`.
    ///
    /// For the different question - "does this game ask for that feature" -
    /// see [`Feature::from_streamline_plugin`].
    pub fn from_runtime(file_name: &str) -> Option<Feature> {
        let lower = file_name.to_ascii_lowercase();
        // Longest first: `nvngx_dlssd` and `nvngx_dlssg` both start with
        // `nvngx_dlss`, so the plain upscaler has to be tested last.
        for (needle, feature) in [
            ("nvngx_dlssnr", Feature::NeuralRendering),
            ("nvngx_dlssd", Feature::RayReconstruction),
            ("nvngx_dlssg", Feature::FrameGeneration),
            ("nvngx_dlss", Feature::SuperResolution),
        ] {
            if lower.starts_with(needle) {
                return Some(feature);
            }
        }
        None
    }

    /// The feature a *Streamline plugin* requests.
    ///
    /// The stronger evidence of the two for what a game does, and the weaker
    /// for what a file provides. `sl.dlss_nr.dll` is in a folder because the
    /// game was built to ask Streamline for feature 1004; `nvngx_dlssnr.dll`
    /// is in a folder for any number of reasons, hand-copying included.
    ///
    /// `sl.common.dll`, `sl.reflex.dll`, `sl.pcl.dll` and `sl.nis.dll` are not
    /// features of their own and return `None`.
    pub fn from_streamline_plugin(file_name: &str) -> Option<Feature> {
        let lower = file_name.to_ascii_lowercase();
        Feature::ALL
            .into_iter()
            .find(|feature| lower == feature.streamline_plugin().0)
    }

    /// Which features the *game itself* feeds, from the runtime files beside
    /// its executable.
    ///
    /// Presence alone is not enough, and getting this wrong was a real bug.
    /// The development machine has a game carrying `nvngx_dlssnr.dll` next to
    /// `nvngx_dlssnr.dll.original` - another tool installed neural rendering
    /// there and kept a backup. Counting that file as evidence the game feeds
    /// neural rendering is exactly backwards: it is evidence somebody *added*
    /// it, which says nothing about whether the renderer tags the G-buffer.
    ///
    /// So only a file that looks like part of the set the game shipped counts.
    /// [`Provenance::ConsistentWithSiblings`] means it matches the versions of
    /// its neighbours, which is what a matched install looks like; anything
    /// added later stands out precisely because it does not.
    pub fn fed_by_game(runtime_files: &[crate::scan::folder::RuntimeFile]) -> Vec<Feature> {
        use crate::scan::folder::Provenance;
        let mut found: Vec<Feature> = runtime_files
            .iter()
            .filter(|file| file.provenance == Provenance::ConsistentWithSiblings)
            .filter_map(|file| {
                let name = file.rel.rsplit(['/', '\\']).next().unwrap_or("");
                // Either kind of evidence that the game drives this feature:
                // the plugin it links to request it, or the runtime that
                // plugin loads.
                Feature::from_streamline_plugin(name).or_else(|| Feature::from_runtime(name))
            })
            .collect();
        found.sort_unstable();
        found.dedup();
        found
    }

    /// Whether a game that feeds `self` is thereby feeding everything `other`
    /// needs.
    ///
    /// This is the question behind "my game has DLSS, so why can I not just
    /// swap in neural rendering?". A game integrated for super resolution tags
    /// depth, motion vectors and jittered colour. It has no reason to tag
    /// albedo, normals or roughness, because nothing it shipped consumed them.
    /// Ray reconstruction does tag those - which is why a game with ray
    /// reconstruction is the one where neural rendering has a real path.
    pub fn satisfies(self, other: Feature) -> bool {
        let mine = self.inputs();
        other.inputs().iter().all(|input| mine.contains(input))
    }
}

/// A resource a feature consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Input {
    Depth,
    MotionVectors,
    /// Colour with the temporal jitter already applied by the renderer.
    JitteredColor,
    /// The sub-pixel offset the renderer used, passed separately.
    JitterOffset,
    /// Post-processed colour with no UI composited into it.
    HudLessColor,
    /// The UI as its own layer, separate from the frame it sits on.
    UiLayer,
    /// The presented backbuffer, tagged so its sub-rectangle is known.
    Backbuffer,
    /// Reflex latency markers, delivered through Streamline.
    ///
    /// Not a buffer at all but a protocol the game takes part in, emitting
    /// present-start and present-end markers carrying the frame index. Frame
    /// generation's guide requires it, and says an existing Reflex integration
    /// that does not go through Streamline cannot be used.
    ReflexMarkers,
    Albedo,
    SpecularAlbedo,
    NormalsRoughness,
    /// View and clip matrices, row-major and jitter-free.
    CameraMatrices,
}

/// How well an input can be supplied when the game does not provide it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Recovery {
    /// Available or computable to a standard the feature can use.
    Sound,
    /// Obtainable, but as an estimate. This is where quality varies between
    /// games, and it is worth naming rather than averaging away.
    Estimated,
    /// Cannot be imposed from outside: it would require the game to render
    /// differently, not merely to hand something over.
    NeedsRendererChange,
}

impl Input {
    pub const fn label(self) -> &'static str {
        match self {
            Input::Depth => "depth buffer",
            Input::MotionVectors => "motion vectors",
            Input::JitteredColor => "jittered colour",
            Input::JitterOffset => "jitter offset",
            Input::HudLessColor => "colour without the UI",
            Input::UiLayer => "the UI as a separate layer",
            Input::Backbuffer => "the presented backbuffer",
            Input::ReflexMarkers => "Reflex latency markers",
            Input::Albedo => "albedo",
            Input::SpecularAlbedo => "specular albedo",
            Input::NormalsRoughness => "normals and roughness",
            Input::CameraMatrices => "camera matrices",
        }
    }

    /// What can be done about this input in a game with no DLSS integration.
    pub const fn recovery(self) -> Recovery {
        match self {
            // ReShade locates the depth buffer, and its heuristics are the
            // most battle-tested part of that project. Not infallible - it can
            // choose the wrong target - but sound when it is right.
            Input::Depth => Recovery::Sound,
            // Computed by optical flow from consecutive frames. Genuinely
            // usable, and genuinely not the renderer's own vectors: it cannot
            // see through transparencies or know about objects that moved
            // without changing pixels.
            Input::MotionVectors => Recovery::Estimated,
            // Derived from the depth buffer and frame-to-frame differences.
            Input::CameraMatrices => Recovery::Estimated,
            // Jitter is a change to how the game samples. Nothing outside the
            // renderer can make it happen, which is why a fed route delivers
            // DLAA at native resolution rather than upscaling.
            Input::JitteredColor | Input::JitterOffset => Recovery::NeedsRendererChange,
            // The UI is composited into the frame before anything outside the
            // renderer sees it. Hooking earlier helps in some engines and not
            // in others.
            Input::HudLessColor | Input::UiLayer => Recovery::Estimated,
            // The backbuffer is the one thing always available: it is what
            // gets presented.
            Input::Backbuffer => Recovery::Sound,
            // Not a buffer but a protocol. The game has to emit present-start
            // and present-end markers with matching frame indices, through
            // Streamline specifically. Nothing outside the renderer can take
            // part on its behalf, so frame generation cannot be fed into a
            // game that lacks it - not approximated, not at all.
            Input::ReflexMarkers => Recovery::NeedsRendererChange,
            // The G-buffer. Albedo cannot be recovered from a finished frame
            // because the lighting has already been applied to it; the same
            // goes for the rest. An estimate can be supplied and the feature
            // will run, but it is running on invented data.
            Input::Albedo | Input::SpecularAlbedo | Input::NormalsRoughness => Recovery::Estimated,
        }
    }
}

/// How a feature would be supplied in a given game.
///
/// Ordered best to worst, so a list of outlooks sorts into the order a user
/// wants to read it: what works properly first, what cannot be done last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Quality {
    /// The game provides every input itself. Swapping the runtime is the whole
    /// job and the result is what the developer shipped, only newer.
    Native,
    /// The inputs are there but the graphics card cannot execute the feature.
    ///
    /// Listed before the input-related verdicts because it is decided first:
    /// there is no point discussing what a game feeds if the hardware cannot
    /// run the thing being fed.
    HardwareTooOld,
    /// The game's own DLSS is mirrored onto a private session, so the inputs
    /// are real rather than estimated.
    Mirrored,
    /// Some inputs are estimated. Works, and how well is game-dependent.
    Estimated,
    /// Cannot be delivered: an input would require the game to render
    /// differently.
    OutOfReach,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outlook {
    pub feature: Feature,
    pub quality: Quality,
    pub route: Route,
    /// Inputs that would be estimated rather than real, worst first.
    pub estimated: Vec<Input>,
    /// Inputs that cannot be supplied at all.
    pub out_of_reach: Vec<Input>,
    /// One sentence for the user. Says what they get, not what the code did.
    pub note: String,
    /// False when we are reasoning from inference rather than documentation.
    pub documented: bool,
    /// Set when the feature needs an add-on to invoke it, whatever the game
    /// provides.
    ///
    /// Deliberately separate from `quality`, because they answer different
    /// questions. Quality is "how good will it look" - a game that feeds the
    /// G-buffer gives neural rendering real data. This is "what do I have to
    /// install", and for neural rendering the answer is always "more than the
    /// runtime", because no game can ask for it directly.
    pub needs_consumer_addon: Option<String>,
}

/// What each feature would look like in this game, on this route.
pub fn outlook(
    feature: Feature,
    _integration: Integration,
    route: Route,
    game_feeds: &[Feature],
    card: Option<Generation>,
) -> Outlook {
    let documented = feature.requirements_documented();
    let fed_by_game = game_feeds.iter().any(|have| have.satisfies(feature));
    // A feature the game does not itself request needs something to request it
    // on the game's behalf. This used to be hard-coded for neural rendering on
    // the false premise that no game could ever ask for it; it is now decided
    // by what the game ships, which is both correct and the same rule for
    // every feature.
    let needs_consumer_addon = (!game_feeds.contains(&feature)).then(|| {
        let (plugin, id) = feature.streamline_plugin();
        format!(
            "This game does not ship {plugin}, so it never asks Streamline for {} \
             (feature {id}) - something has to request it on the game's behalf.",
            feature.label()
        )
    });

    // The hardware gate is independent of the input contract and is decided
    // first: what a game feeds is beside the point if the card cannot execute
    // the feature. Passed in rather than read here, so this stays a pure
    // function of its arguments - the same reason the planner does not consult
    // the filesystem. `None` means the card is unknown, which reports on the
    // inputs instead of refusing: an unidentified adapter is not evidence of
    // old hardware.
    if let Some(card) = card {
        if !card.at_least(feature.minimum_generation()) {
            return Outlook {
                feature,
                quality: Quality::HardwareTooOld,
                route,
                estimated: Vec::new(),
                out_of_reach: Vec::new(),
                note: format!(
                    "{} needs {} or newer, and this machine has {}.",
                    feature.label(),
                    feature.minimum_generation().label(),
                    card.label()
                ),
                documented,
                needs_consumer_addon,
            };
        }
    }

    if route == Route::NativeSwap {
        // The game satisfies this feature's contract already. Nothing is
        // estimated, and a newer runtime simply does its job better.
        if fed_by_game {
            return Outlook {
                feature,
                quality: Quality::Native,
                route,
                estimated: Vec::new(),
                out_of_reach: Vec::new(),
                note: if needs_consumer_addon.is_some() {
                    // The game produces everything the feature consumes, but
                    // it never asks for the feature - so the runtime alone
                    // changes nothing. Saying "just replace the runtime" here
                    // was the wrong sentence, and the one a user would act on.
                    format!(
                        "This game produces everything {} needs, so it will run on real data - \
                         but it never asks for the feature, so {} has to go in alongside an \
                         add-on that requests {} rather than on its own.",
                        feature.label(),
                        feature.runtime(),
                        feature.streamline_plugin().1
                    )
                } else {
                    format!(
                        "This game feeds {} itself, so replacing {} is all that is needed.",
                        feature.label(),
                        feature.runtime()
                    )
                },
                documented,
                needs_consumer_addon,
            };
        }

        // It has DLSS, but not this feature - so it never tags the resources
        // this one consumes. Swapping the runtime in changes nothing, which is
        // the most commonly misunderstood outcome in this whole space.
        let missing: Vec<Input> = feature
            .inputs()
            .into_iter()
            .filter(|input| !game_feeds.iter().any(|have| have.inputs().contains(input)))
            .collect();
        return Outlook {
            feature,
            quality: Quality::OutOfReach,
            route,
            estimated: Vec::new(),
            out_of_reach: missing.clone(),
            note: if missing.is_empty() {
                format!(
                    "This game does not use {}, so a newer runtime alone will not enable it.",
                    feature.label()
                )
            } else if game_feeds.is_empty() {
                // No feature reads as fed. Saying "this game has DLSS but not
                // X" here would be false: what is actually beside the
                // executable is a runtime file nothing calls - typically one
                // dropped in by hand, which is why it passed the file check
                // and failed the provenance one.
                format!(
                    "A DLSS runtime file is present, but nothing in this game drives it: it \
                     never produces {}. Putting {} beside it would change nothing - a file in \
                     the folder is not the same as the renderer using it, and this feature \
                     needs one of the other routes.",
                    list(&missing),
                    feature.runtime()
                )
            } else {
                format!(
                    "This game has DLSS but not {}, so it never produces {}. Putting {} beside \
                     it would change nothing - this feature needs one of the other routes.",
                    feature.label(),
                    list(&missing),
                    feature.runtime()
                )
            },
            documented,
            needs_consumer_addon,
        };
    }

    // The bridge mirrors a real DLSS session, so the inputs it forwards are
    // the game's own. What it cannot do is invent inputs the game never had.
    if route == Route::Bridge {
        return Outlook {
            feature,
            quality: Quality::Mirrored,
            route,
            estimated: Vec::new(),
            out_of_reach: Vec::new(),
            note: format!(
                "This game's own DLSS is mirrored onto a private DirectX 12 session, so {} \
                 runs on the game's real data.",
                feature.label()
            ),
            documented,
            needs_consumer_addon,
        };
    }

    // Everything else is fed. Sort the inputs by how well they can be
    // supplied, and let the worst one decide the verdict.
    let mut estimated: Vec<Input> = Vec::new();
    let mut out_of_reach: Vec<Input> = Vec::new();
    for input in feature.inputs() {
        match input.recovery() {
            Recovery::Sound => {}
            Recovery::Estimated => estimated.push(input),
            Recovery::NeedsRendererChange => out_of_reach.push(input),
        }
    }
    estimated.sort_unstable();
    out_of_reach.sort_unstable();

    // Jitter is the interesting case. Super resolution without it is not
    // broken - it is DLAA, which is the same network at native resolution.
    // Saying "unavailable" would be wrong; saying "upscaling" would also be
    // wrong.
    let jitter_only = !out_of_reach.is_empty()
        && out_of_reach
            .iter()
            .all(|input| matches!(input, Input::JitteredColor | Input::JitterOffset));

    let (quality, note) = if jitter_only {
        (
            Quality::Estimated,
            format!(
                "{} can run, but at native resolution rather than upscaling: jitter is part of \
                 how a game samples and cannot be added from outside.",
                feature.label()
            ),
        )
    } else if !out_of_reach.is_empty() {
        (
            Quality::OutOfReach,
            format!(
                "{} needs {} from inside the renderer, which cannot be supplied here.",
                feature.label(),
                list(&out_of_reach)
            ),
        )
    } else if estimated.is_empty() {
        (
            Quality::Native,
            format!("{} has everything it needs.", feature.label()),
        )
    } else {
        (
            Quality::Estimated,
            format!(
                "{} will run on estimated {}, so how well it looks depends on the game.",
                feature.label(),
                list(&estimated)
            ),
        )
    };

    Outlook {
        feature,
        quality,
        route,
        estimated,
        out_of_reach,
        note,
        documented,
        needs_consumer_addon,
    }
}

/// Every feature's outlook, best prospects first.
pub fn all_outlooks(
    integration: Integration,
    route: Route,
    game_feeds: &[Feature],
    card: Option<Generation>,
) -> Vec<Outlook> {
    let mut found: Vec<Outlook> = Feature::ALL
        .iter()
        .map(|feature| outlook(*feature, integration, route, game_feeds, card))
        .collect();
    found.sort_by_key(|entry| (entry.quality, entry.feature));
    found
}

fn list(inputs: &[Input]) -> String {
    let names: Vec<&str> = inputs.iter().map(|input| input.label()).collect();
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_streamline_game_gets_everything_natively() {
        for feature in Feature::ALL {
            let found = outlook(
                feature,
                Integration::Streamline,
                Route::NativeSwap,
                &Feature::ALL,
                Some(Generation::Blackwell),
            );
            assert_eq!(found.quality, Quality::Native, "{feature:?}");
            assert!(found.estimated.is_empty());
            assert!(found.out_of_reach.is_empty());
            assert!(found.note.contains(feature.runtime()));
        }
    }

    #[test]
    fn neural_rendering_always_needs_an_add_on_however_good_the_game_is() {
        // A game that feeds ray reconstruction produces everything neural
        // rendering consumes, so it gives it *real data* - the quality is
        // native. But feeding a feature is not requesting it: unless the game
        // ships `sl.dlss_nr.dll` it never asks Streamline for feature 1004,
        // and the runtime alone does nothing.
        //
        // The two are deliberately separate answers: "how good will it look"
        // and "what do I have to install".
        let found = outlook(
            Feature::NeuralRendering,
            Integration::Streamline,
            Route::NativeSwap,
            &[Feature::RayReconstruction],
            Some(Generation::Blackwell),
        );
        assert_eq!(found.quality, Quality::Native, "the data is real");
        let reason = found
            .needs_consumer_addon
            .as_deref()
            .expect("a game that never requests the feature needs a consumer");
        assert!(reason.contains("sl.dlss_nr.dll"), "{reason}");
        assert!(reason.contains("1004"), "{reason}");
        // And the sentence must not tell somebody a runtime swap suffices.
        assert!(!found.note.contains("all that is needed"));
        assert!(found.note.contains("rather than on its own"));
    }

    #[test]
    fn a_game_that_ships_the_neural_plugin_needs_no_add_on() {
        // The correction that motivated the rule above. Neural rendering is
        // Streamline feature 1004, shipped as `sl.dlss_nr.dll` from 2.13 - so
        // a game built against 2.13 requests it like anything else, and there
        // is nothing for an add-on to do. Reporting otherwise would send a
        // user to install a consumer their game does not need.
        let found = outlook(
            Feature::NeuralRendering,
            Integration::Streamline,
            Route::NativeSwap,
            &[Feature::NeuralRendering, Feature::RayReconstruction],
            Some(Generation::Blackwell),
        );

        assert_eq!(found.quality, Quality::Native);
        assert!(found.needs_consumer_addon.is_none());
        assert!(found.note.contains("all that is needed"), "{}", found.note);
    }

    #[test]
    fn the_streamline_plugin_is_stronger_evidence_than_the_runtime_file() {
        // `sl.dlss_nr.dll` is there because the game was built to ask for
        // feature 1004; `nvngx_dlssnr.dll` is the model it loads, and gets
        // copied into folders by hand all the time. So the plugin name has to
        // resolve, and it has to resolve to the same feature.
        assert_eq!(
            Feature::from_streamline_plugin("sl.dlss_nr.dll"),
            Some(Feature::NeuralRendering)
        );
        assert_eq!(
            Feature::from_streamline_plugin("sl.dlss_g.dll"),
            Some(Feature::FrameGeneration)
        );
        assert_eq!(
            Feature::from_streamline_plugin("sl.dlss.dll"),
            Some(Feature::SuperResolution)
        );
        // `sl.common.dll` and `sl.reflex.dll` are not features of their own.
        assert_eq!(Feature::from_streamline_plugin("sl.common.dll"), None);
        assert_eq!(Feature::from_streamline_plugin("sl.reflex.dll"), None);

        // And the two lookups must not bleed into each other. A plugin is not
        // a runtime: offering `sl.dlss.dll` as something to install as
        // `nvngx_dlss.dll` would write 400 KB of brokering over a 70 MB model.
        assert_eq!(Feature::from_runtime("sl.dlss.dll"), None);
        assert_eq!(Feature::from_runtime("sl.dlss_nr.dll"), None);
        assert_eq!(Feature::from_streamline_plugin("nvngx_dlss.dll"), None);
    }

    #[test]
    fn only_frame_generation_and_neural_rendering_refuse_direct3d_11() {
        // From the plugins' own `rhi` manifests. Ray reconstruction reads as
        // d3d12-only if you reason by analogy with the other 1000-series
        // features; `sl.dlss_d`'s manifest says `d3d11, d3d12, vk`, so it is
        // pinned here to stop the analogy creeping back in.
        assert!(!Feature::SuperResolution.requires_modern_rhi());
        assert!(!Feature::RayReconstruction.requires_modern_rhi());
        for feature in [Feature::FrameGeneration, Feature::NeuralRendering] {
            assert!(feature.requires_modern_rhi(), "{feature:?}");
        }
    }

    #[test]
    fn super_resolution_without_integration_is_dlaa_not_upscaling() {
        // The distinction that matters most and is most often fudged. Jitter
        // is a change to the game's own sampling, so it cannot be imposed -
        // which makes this the same network at native resolution.
        let found = outlook(
            Feature::SuperResolution,
            Integration::None,
            Route::Feeder,
            &[],
            Some(Generation::Blackwell),
        );
        assert_eq!(found.quality, Quality::Estimated);
        assert!(found.out_of_reach.contains(&Input::JitteredColor));
        assert!(found
            .note
            .contains("native resolution rather than upscaling"));
    }

    #[test]
    fn neural_rendering_on_the_feeder_route_runs_on_estimated_g_buffer() {
        // It does work - the community routes prove that - but albedo cannot
        // be recovered from a finished frame, so it is estimated. Naming which
        // inputs are invented is the honest version of "results vary".
        let found = outlook(
            Feature::NeuralRendering,
            Integration::None,
            Route::Feeder,
            &[],
            Some(Generation::Blackwell),
        );
        assert_eq!(found.quality, Quality::Estimated);
        for input in [
            Input::Albedo,
            Input::SpecularAlbedo,
            Input::NormalsRoughness,
        ] {
            assert!(found.estimated.contains(&input), "{input:?}");
        }
        assert!(!found.documented, "NR's requirements are not published");
    }

    #[test]
    fn the_bridge_supplies_real_data_rather_than_estimates() {
        let found = outlook(
            Feature::NeuralRendering,
            Integration::None,
            Route::Bridge,
            &[],
            Some(Generation::Blackwell),
        );
        assert_eq!(found.quality, Quality::Mirrored);
        assert!(found.estimated.is_empty());
        assert!(found.note.contains("real data"));
    }

    #[test]
    fn frame_generation_cannot_be_fed_into_a_game_that_lacks_it() {
        // A correction to something this module first got wrong. Frame
        // generation's own guide requires Reflex integrated *through
        // Streamline*, and says a plain Reflex integration will not do. That
        // is a protocol the game takes part in - emitting present markers with
        // matching frame indices - not a buffer anyone can estimate. So this
        // is out of reach rather than approximate, and saying "estimated"
        // would have promised something impossible.
        let found = outlook(
            Feature::FrameGeneration,
            Integration::None,
            Route::Feeder,
            &[],
            Some(Generation::Blackwell),
        );
        assert_eq!(found.quality, Quality::OutOfReach);
        assert!(found.out_of_reach.contains(&Input::ReflexMarkers));
        assert!(found.note.contains("Reflex latency markers"));

        // It also wants the UI as its own layer as well as a HUD-less frame.
        let inputs = Feature::FrameGeneration.inputs();
        assert!(inputs.contains(&Input::UiLayer));
        assert!(inputs.contains(&Input::HudLessColor));
        assert!(inputs.contains(&Input::Backbuffer));
    }

    #[test]
    fn every_feature_names_its_runtime_and_needs_the_camera_matrices() {
        for feature in Feature::ALL {
            assert!(feature.runtime().starts_with("nvngx_"));
            assert!(
                feature.inputs().contains(&Input::CameraMatrices),
                "{feature:?} - every SL feature requires the camera constants"
            );
            assert!(feature.inputs().contains(&Input::Depth));
        }
    }

    #[test]
    fn only_neural_rendering_is_flagged_as_undocumented() {
        assert!(!Feature::NeuralRendering.requirements_documented());
        for feature in [
            Feature::SuperResolution,
            Feature::RayReconstruction,
            Feature::FrameGeneration,
        ] {
            assert!(feature.requirements_documented(), "{feature:?}");
        }
    }

    #[test]
    fn outlooks_are_ordered_with_the_best_prospects_first() {
        let native = all_outlooks(
            Integration::Streamline,
            Route::NativeSwap,
            &Feature::ALL,
            Some(Generation::Blackwell),
        );
        assert!(native.iter().all(|entry| entry.quality == Quality::Native));

        let fed = all_outlooks(
            Integration::None,
            Route::Feeder,
            &[],
            Some(Generation::Blackwell),
        );
        assert_eq!(fed.len(), 4);

        // Not everything degrades gracefully, and an earlier version of this
        // test asserted that it did. Frame generation is genuinely out of
        // reach: it needs Reflex integrated through Streamline, which is a
        // protocol the game must take part in rather than a buffer anyone can
        // estimate. The other three can run on estimates.
        assert_eq!(
            fed.iter()
                .filter(|entry| entry.quality == Quality::Estimated)
                .map(|entry| entry.feature)
                .collect::<Vec<_>>(),
            vec![
                Feature::SuperResolution,
                Feature::RayReconstruction,
                Feature::NeuralRendering
            ]
        );
        assert_eq!(
            fed.iter()
                .filter(|entry| entry.quality == Quality::OutOfReach)
                .map(|entry| entry.feature)
                .collect::<Vec<_>>(),
            vec![Feature::FrameGeneration]
        );

        // Better prospects sort first, and each says something specific.
        assert!(fed
            .windows(2)
            .all(|pair| pair[0].quality <= pair[1].quality));
        assert!(fed.iter().all(|entry| !entry.note.is_empty()));
    }

    #[test]
    fn a_game_with_only_upscaling_cannot_be_given_neural_rendering_by_a_swap() {
        // The most misunderstood outcome in this space, and the reason this
        // module exists. A game integrated for super resolution tags depth,
        // motion vectors and jittered colour. It has no reason to tag albedo
        // or normals, because nothing it shipped consumed them - so a newer
        // runtime sitting beside it has nothing feeding it.
        let feeds = [Feature::SuperResolution];
        let found = outlook(
            Feature::NeuralRendering,
            Integration::Streamline,
            Route::NativeSwap,
            &feeds,
            Some(Generation::Blackwell),
        );
        assert_eq!(found.quality, Quality::OutOfReach);
        assert!(found.out_of_reach.contains(&Input::Albedo));
        assert!(found.note.contains("would change nothing"));

        // Super resolution itself is of course fine.
        let same = outlook(
            Feature::SuperResolution,
            Integration::Streamline,
            Route::NativeSwap,
            &feeds,
            Some(Generation::Blackwell),
        );
        assert_eq!(same.quality, Quality::Native);
    }

    #[test]
    fn ray_reconstruction_is_what_opens_the_door_to_neural_rendering() {
        // RR tags the G-buffer, so a game that has it is already producing
        // what neural rendering needs. This is why the flagship NR titles are
        // the path-traced ones.
        assert!(Feature::RayReconstruction.satisfies(Feature::NeuralRendering));
        assert!(!Feature::SuperResolution.satisfies(Feature::NeuralRendering));
        // And frame generation is its own thing: it needs a UI-less frame that
        // neither of the others produces.
        assert!(!Feature::SuperResolution.satisfies(Feature::FrameGeneration));

        let found = outlook(
            Feature::NeuralRendering,
            Integration::Streamline,
            Route::NativeSwap,
            &[Feature::RayReconstruction],
            Some(Generation::Blackwell),
        );
        assert_eq!(found.quality, Quality::Native);
    }

    #[test]
    fn a_runtime_somebody_else_installed_is_not_evidence_the_game_feeds_it() {
        // The bug this guards, found against a real game folder: a title with
        // `nvngx_dlssnr.dll` beside `nvngx_dlssnr.dll.original` had neural
        // rendering installed by another tool. Reading that as "the game feeds
        // neural rendering" is backwards - it is evidence somebody added it.
        use crate::scan::folder::{Provenance, RuntimeFile, RuntimeKind};

        let shipped = |rel: &str| RuntimeFile {
            rel: rel.to_owned(),
            kind: RuntimeKind::Dlss,
            version: Some("310.1.0.0".to_owned()),
            provenance: Provenance::ConsistentWithSiblings,
        };
        let added = |rel: &str, provenance: Provenance| RuntimeFile {
            rel: rel.to_owned(),
            kind: RuntimeKind::Dlss,
            version: Some("310.8.0.0".to_owned()),
            provenance,
        };

        let files = vec![
            shipped("bin/x64/nvngx_dlss.dll"),
            added(
                "bin/x64/nvngx_dlssnr.dll",
                Provenance::VersionDiffersFromSiblings,
            ),
            added("bin/x64/nvngx_dlssg.dll", Provenance::OurInstall),
        ];
        let feeds = Feature::fed_by_game(&files);
        assert_eq!(feeds, vec![Feature::SuperResolution]);

        // And so neural rendering is reported as out of reach on a swap,
        // rather than as already working.
        let found = outlook(
            Feature::NeuralRendering,
            Integration::Streamline,
            Route::NativeSwap,
            &feeds,
            Some(Generation::Blackwell),
        );
        assert_eq!(found.quality, Quality::OutOfReach);
    }

    #[test]
    fn features_are_recognised_from_their_runtime_filenames() {
        // The prefixes overlap, so order matters: nvngx_dlssd and nvngx_dlssg
        // both begin with nvngx_dlss.
        assert_eq!(
            Feature::from_runtime("nvngx_dlss.dll"),
            Some(Feature::SuperResolution)
        );
        assert_eq!(
            Feature::from_runtime("nvngx_dlssd.dll"),
            Some(Feature::RayReconstruction)
        );
        assert_eq!(
            Feature::from_runtime("nvngx_dlssg.dll"),
            Some(Feature::FrameGeneration)
        );
        assert_eq!(
            Feature::from_runtime("NVNGX_DLSSNR.DLL"),
            Some(Feature::NeuralRendering)
        );
        assert_eq!(Feature::from_runtime("sl.dlss.dll"), None);
        assert_eq!(Feature::from_runtime("d3d12.dll"), None);
    }

    #[test]
    fn hardware_is_a_separate_gate_from_what_the_game_feeds() {
        // Two independent conditions, and conflating them is how somebody gets
        // told a feature is available when their card cannot execute it. A
        // Turing card feeds nothing differently - it simply cannot run frame
        // generation, which needs the optical flow accelerator Ada introduced.
        let turing = outlook(
            Feature::FrameGeneration,
            Integration::Streamline,
            Route::NativeSwap,
            &Feature::ALL,
            Some(Generation::Turing),
        );
        assert_eq!(turing.quality, Quality::HardwareTooOld);
        assert!(turing.note.contains("RTX 40-series"));

        // The same game and the same route on an Ada card is fine.
        let ada = outlook(
            Feature::FrameGeneration,
            Integration::Streamline,
            Route::NativeSwap,
            &Feature::ALL,
            Some(Generation::Ada),
        );
        assert_eq!(ada.quality, Quality::Native);

        // Neural rendering is gated higher still.
        let nr_on_ada = outlook(
            Feature::NeuralRendering,
            Integration::Streamline,
            Route::NativeSwap,
            &Feature::ALL,
            Some(Generation::Ada),
        );
        assert_eq!(nr_on_ada.quality, Quality::HardwareTooOld);
    }

    #[test]
    fn an_unknown_card_reports_on_inputs_rather_than_refusing() {
        // Consistent with every other check in this codebase: not being able
        // to identify hardware is not evidence that it is too old.
        let found = outlook(
            Feature::NeuralRendering,
            Integration::Streamline,
            Route::NativeSwap,
            &Feature::ALL,
            None,
        );
        assert_eq!(found.quality, Quality::Native);

        // And an unrecognised NVIDIA card answers `at_least` true, so it takes
        // the same path.
        let unknown = outlook(
            Feature::NeuralRendering,
            Integration::Streamline,
            Route::NativeSwap,
            &Feature::ALL,
            Some(Generation::Unknown),
        );
        assert_eq!(unknown.quality, Quality::Native);
    }

    #[test]
    fn the_frame_generation_gate_is_what_the_rtx_40_unlock_addresses() {
        // Worth pinning because it is easy to misread. Frame generation needs
        // Ada; the unlock raises the *multiplier* on Ada cards. It does not
        // bring the feature to Turing or Ampere, and nothing does.
        assert_eq!(
            Feature::FrameGeneration.minimum_generation(),
            Generation::Ada
        );
        assert!(!Generation::Ampere.at_least(Generation::Ada));
        assert!(Generation::Ada.at_least(Generation::Ada));

        // Super resolution and ray reconstruction go back to Turing.
        for feature in [Feature::SuperResolution, Feature::RayReconstruction] {
            assert_eq!(feature.minimum_generation(), Generation::Turing);
        }
    }

    #[test]
    fn the_input_list_reads_as_a_sentence() {
        assert_eq!(list(&[]), "");
        assert_eq!(list(&[Input::Depth]), "depth buffer");
        assert_eq!(
            list(&[Input::Depth, Input::Albedo]),
            "depth buffer and albedo"
        );
        assert_eq!(
            list(&[Input::Depth, Input::Albedo, Input::MotionVectors]),
            "depth buffer, albedo and motion vectors"
        );
    }
}
