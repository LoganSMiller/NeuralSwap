//! What a component is, where it comes from, and what we are allowed to do
//! with it.
//!
//! Every tool in this space needs the same dozen third-party pieces, and each
//! one solves the sourcing question by hand. Two of them bundle everything and
//! hope; one re-hosts proprietary binaries on its own mirror with no integrity
//! check at all; one fetches each piece from its author but verifies only its
//! own updater.
//!
//! The interesting part is that the *licences differ*, and the correct answer
//! differs with them. ReShade is BSD-3 and may be shipped with its notice.
//! LumeniteFX is "all rights reserved unless explicitly stated" and may only
//! be fetched from the author. NVIDIA's runtimes may be neither shipped nor
//! mirrored by us. Getting that wrong is not a style question.
//!
//! So the licence is part of the data model, and [`Catalog::validate`] refuses
//! a catalogue that pairs a redistribution-restricted component with a source
//! that would redistribute it. The rule is enforced by the type rather than
//! remembered by whoever edits the list next.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{fail, Code, Result};

/// The current shape of a catalogue document. A catalogue from a newer build
/// is refused rather than half-read.
pub const CATALOG_VERSION: u32 = 1;

/// What a component does in an install, which decides where its files go and
/// what else has to be present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    /// The injector everything else loads through.
    Injector,
    /// A ReShade add-on: an `.addon64` beside the executable.
    Addon,
    /// An ASI plugin, loaded by Ultimate ASI Loader rather than by ReShade.
    AsiPlugin,
    /// Shaders and textures, into `reshade-shaders/`.
    Shaders,
    /// An upscaler runtime - the `nvngx_*` and `sl.*` families.
    Runtime,
    /// A translation or compatibility layer.
    Compat,
    /// A loader another component depends on.
    Loader,
}

/// The redistribution terms, as they bear on what we may do with the bytes.
///
/// Not a full licence model - only the question this application has to
/// answer, which is whether we may put a copy of this in our own installer or
/// on our own server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Licence {
    Mit,
    Bsd3,
    Gpl3,
    /// Anything else, with the answer stated explicitly and a reason recorded.
    /// The reason is shown to the user, so it has to be true and readable.
    Other {
        name: String,
        redistributable: bool,
        why: String,
    },
}

impl Licence {
    /// Whether we may ship or mirror a copy ourselves.
    pub fn redistributable(&self) -> bool {
        match self {
            // All three permit binary redistribution. BSD-3 and MIT require
            // the notice to travel with it; GPL-3 requires rather more, which
            // is why a GPL component is fetched rather than linked into us.
            Licence::Mit | Licence::Bsd3 | Licence::Gpl3 => true,
            Licence::Other {
                redistributable, ..
            } => *redistributable,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Licence::Mit => "MIT".to_owned(),
            Licence::Bsd3 => "BSD 3-Clause".to_owned(),
            Licence::Gpl3 => "GPL-3.0".to_owned(),
            Licence::Other { name, .. } => name.clone(),
        }
    }

    /// Whether the notice has to be shipped alongside the bytes.
    pub fn needs_notice(&self) -> bool {
        matches!(self, Licence::Mit | Licence::Bsd3 | Licence::Gpl3)
            || matches!(self, Licence::Other { .. })
    }
}

/// Where a component's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Source {
    /// Never fetched and never shipped: it is already on the user's machine,
    /// and we help them find their own copy.
    ///
    /// This is the honest answer for a proprietary runtime with no public
    /// distribution. The alternative - mirroring it - is what the rest of this
    /// ecosystem does, and it is redistribution however it is dressed up.
    LocalOnly { hint: String },
    /// Published somewhere we cannot fetch from, so the user brings it.
    ///
    /// Several of the add-ons this ecosystem depends on are distributed
    /// through Discord rather than a release page - there is no URL to pin, no
    /// digest to publish and nothing to automate. Saying so, and naming the
    /// files expected, is more use than pretending a download exists.
    UserObtained {
        /// Where to get it, in words - a channel name, an invite link.
        from: String,
        /// The files that should end up in the folder the user chooses.
        files: Vec<String>,
    },
    /// Shipped inside our own installer. Only legitimate for a licence that
    /// permits it.
    Bundled { rel: String },
    /// A fixed URL whose digest we published. The strongest fetched form: the
    /// bytes are known before they arrive.
    Pinned {
        url: String,
        sha256: String,
        size: u64,
    },
    /// The newest release of a repository, asset chosen by filename suffix.
    ///
    /// A moving target, so it cannot be pinned. The digest is recorded the
    /// first time and compared on every fetch after that.
    GitHubLatest { repo: String, asset_suffix: String },
    /// A branch archive, for projects published without releases.
    GitHubBranch { repo: String, branch: String },
    /// The author's own distribution, with a version substituted into the URL.
    /// `known` is the versions we have seen, newest first.
    Official {
        template: String,
        known: Vec<String>,
    },
}

/// How much is known about the bytes before they are used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Trust {
    /// Shipped by us; there is nothing to verify at runtime.
    Bundled,
    /// Compared against a digest we published.
    Pinned,
    /// Recorded on the first successful fetch and compared on every one after.
    ///
    /// Trust on first use. Weaker than a pin - it cannot tell you the first
    /// download was genuine - but it detects a release quietly replaced later,
    /// which is the realistic supply-chain event for a moving target and which
    /// nothing else in this space checks at all.
    FirstUse,
    /// Nothing to fetch: the user supplies it.
    UserSupplied,
}

impl Source {
    pub fn trust(&self) -> Trust {
        match self {
            Source::LocalOnly { .. } | Source::UserObtained { .. } => Trust::UserSupplied,
            Source::Bundled { .. } => Trust::Bundled,
            Source::Pinned { .. } => Trust::Pinned,
            Source::GitHubLatest { .. } | Source::GitHubBranch { .. } | Source::Official { .. } => {
                Trust::FirstUse
            }
        }
    }

    /// Whether using this source means *we* distribute the bytes.
    ///
    /// Fetching from the author's own server does not: the bytes travel from
    /// them to the user, and we are the thing that knew the URL. Shipping a
    /// copy does, and so would mirroring one.
    pub fn we_redistribute(&self) -> bool {
        matches!(self, Source::Bundled { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Component {
    pub id: String,
    pub name: String,
    /// One line, shown in the picker. Plain language, no jargon.
    pub summary: String,
    pub role: Role,
    pub licence: Licence,
    /// Where a user can read about it and check we are telling the truth.
    pub homepage: String,
    pub source: Source,
    /// Components that must be installed for this one to do anything.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Components that must **not** be installed alongside this one.
    ///
    /// The reason this is data rather than a note in a README: the failures
    /// are silent. Two neural consumers beside each other and the first does
    /// nothing for the entire session, with no error anywhere. Feeder and the
    /// DX11 bridge both installed for one game is explicitly warned against by
    /// both authors. OptiScaler left enabled breaks the Feeder route. None of
    /// these announce themselves, so the preflight has to.
    #[serde(default)]
    pub conflicts_with: Vec<String>,
    /// Marked when a component is experimental or known to be rough. The UI
    /// says so rather than letting somebody find out.
    #[serde(default)]
    pub experimental: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub version: u32,
    pub components: BTreeMap<String, Component>,
}

impl Catalog {
    pub fn get(&self, id: &str) -> Option<&Component> {
        self.components.get(id)
    }

    pub fn by_role(&self, role: Role) -> Vec<&Component> {
        self.components
            .values()
            .filter(|component| component.role == role)
            .collect()
    }

    /// Check the catalogue is coherent before anything acts on it.
    ///
    /// Run against the built-in list by a test, and against a fetched one
    /// before it is allowed to replace the built-in - a live catalogue is
    /// remote input, and remote input that decides what gets written into a
    /// game folder gets checked.
    pub fn validate(&self) -> Result<()> {
        if self.version > CATALOG_VERSION {
            return fail(
                Code::StateVersionAhead,
                format!(
                    "catalogue version {} is newer than this build",
                    self.version
                ),
            );
        }

        for (key, component) in &self.components {
            if *key != component.id {
                return fail(
                    Code::BadRequest,
                    format!("component keyed as {key} calls itself {}", component.id),
                );
            }
            if component.id.is_empty() || component.name.is_empty() {
                return fail(Code::BadRequest, format!("{key} is missing a name"));
            }

            // The rule this type exists to enforce. A licence that does not
            // permit redistribution cannot be paired with a source that
            // redistributes, however convenient that would be.
            if component.source.we_redistribute() && !component.licence.redistributable() {
                return fail(
                    Code::BadRequest,
                    format!(
                        "{key} is licensed {} and cannot be bundled - fetch it from the author instead",
                        component.licence.label()
                    ),
                );
            }

            if let Source::Pinned { sha256, .. } = &component.source {
                if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return fail(
                        Code::BadRequest,
                        format!("{key} has a pinned source with no usable digest"),
                    );
                }
            }
            if let Source::Official { template, .. } = &component.source {
                if !template.contains("{version}") {
                    return fail(
                        Code::BadRequest,
                        format!("{key} has an official source with no version placeholder"),
                    );
                }
            }
            for url in component.source.urls() {
                if !url.starts_with("https://") {
                    return fail(
                        Code::BadRequest,
                        format!("{key} would be fetched over something other than HTTPS"),
                    );
                }
            }

            for needed in &component.requires {
                if !self.components.contains_key(needed) {
                    return fail(
                        Code::BadRequest,
                        format!("{key} requires {needed}, which is not in the catalogue"),
                    );
                }
                if needed == &component.id {
                    return fail(Code::BadRequest, format!("{key} requires itself"));
                }
            }

            for rival in &component.conflicts_with {
                let Some(other) = self.components.get(rival) else {
                    return fail(
                        Code::BadRequest,
                        format!("{key} conflicts with {rival}, which is not in the catalogue"),
                    );
                };
                // Conflict is a property of a pair, so it has to be declared
                // by both. Otherwise the warning a user sees would depend on
                // which of the two they happened to install second.
                if !other.conflicts_with.contains(&component.id) {
                    return fail(
                        Code::BadRequest,
                        format!("{key} conflicts with {rival} but {rival} does not say so"),
                    );
                }
                if component.requires.contains(rival) {
                    return fail(
                        Code::BadRequest,
                        format!("{key} both requires and conflicts with {rival}"),
                    );
                }
            }
        }
        Ok(())
    }
}

impl Source {
    /// Every URL this source could reach, for the HTTPS check. A template is
    /// rendered with a placeholder version so the scheme can still be seen.
    fn urls(&self) -> Vec<String> {
        match self {
            Source::LocalOnly { .. } | Source::UserObtained { .. } | Source::Bundled { .. } => {
                Vec::new()
            }
            Source::Pinned { url, .. } => vec![url.clone()],
            Source::GitHubLatest { repo, .. } => {
                vec![format!(
                    "https://api.github.com/repos/{repo}/releases/latest"
                )]
            }
            Source::GitHubBranch { repo, branch } => vec![format!(
                "https://codeload.github.com/{repo}/zip/refs/heads/{branch}"
            )],
            Source::Official { template, .. } => vec![template.replace("{version}", "0")],
        }
    }
}

fn other(name: &str, redistributable: bool, why: &str) -> Licence {
    Licence::Other {
        name: name.to_owned(),
        redistributable,
        why: why.to_owned(),
    }
}

fn component(
    id: &str,
    name: &str,
    summary: &str,
    role: Role,
    licence: Licence,
    homepage: &str,
    source: Source,
) -> Component {
    Component {
        id: id.to_owned(),
        name: name.to_owned(),
        summary: summary.to_owned(),
        role,
        licence,
        homepage: homepage.to_owned(),
        source,
        requires: Vec::new(),
        conflicts_with: Vec::new(),
        experimental: false,
    }
}

/// The catalogue this build ships with.
///
/// Every licence here was read rather than assumed, and every source points at
/// the party that publishes the thing. Where a component is fetched from a
/// moving target that is stated as such, and where it cannot be fetched at all
/// that is stated too.
pub fn default_catalog() -> Catalog {
    // Built by pushing rather than as one literal: the entries are grouped by
    // what they do, with the reasoning for each sitting beside it, and several
    // need a line or two of adjustment after construction.
    let mut components: Vec<Component> = Vec::with_capacity(16);

    // ---------------------------------------------------------- the injector
    components.push(component(
        "reshade",
        "ReShade (with add-on support)",
        "The injector every add-on below loads through. Downloaded from the author's own site.",
        Role::Injector,
        Licence::Bsd3,
        "https://reshade.me",
        // Fetched from reshade.me rather than shipped. BSD-3 would permit
        // shipping it with the notice, but the author's site is canonical and
        // stays current, and an installer that is always the latest is worth
        // more than one frozen at whatever we built against.
        Source::Official {
            template: "https://reshade.me/downloads/ReShade_Setup_{version}_Addon.exe".to_owned(),
            known: [
                "6.8.0", "6.7.3", "6.7.2", "6.7.1", "6.7.0", "6.6.2", "6.6.1", "6.6.0",
            ]
            .iter()
            .map(|version| (*version).to_owned())
            .collect(),
        },
    ));

    // ------------------------------------------------------ the DLSS 5 routes
    components.push(component(
        "dlss5-feeder",
        "DLSS 5 Feeder",
        "Brings neural rendering to games with no DLSS at all, by building the inputs it needs from ReShade's depth and motion vectors.",
        Role::Addon,
        Licence::Mit,
        "https://github.com/jlrouzies-fr/DLSS5-Feeder",
        Source::GitHubLatest {
            repo: "jlrouzies-fr/DLSS5-Feeder".to_owned(),
            asset_suffix: ".zip".to_owned(),
        },
    ));

    components.push(component(
        "dlss5-bridge",
        "DLSS 5 DX11 Bridge",
        "Brings neural rendering to DirectX 11 and Vulkan games that already have DLSS, by mirroring it onto a private DirectX 12 session.",
        Role::Addon,
        Licence::Mit,
        "https://github.com/NIGos/dlss5-bridge",
        Source::GitHubLatest {
            repo: "NIGos/dlss5-bridge".to_owned(),
            asset_suffix: ".addon64".to_owned(),
        },
    ));

    // Three different add-ons that are easy to confuse, and confusing them is
    // how an install silently does nothing. Feeder's own README spends a
    // section disambiguating them, and an earlier version of this catalogue
    // got it wrong: it listed one "RenoDX" fetched from GitHub releases, which
    // is neither of the two that matter here and is not published there.
    components.push(component(
        "renodx",
        "RenoDX",
        "Tone mapping and HDR for games that shipped without it. The general-purpose add-on, and not one of the neural-rendering pieces.",
        Role::Addon,
        Licence::Mit,
        "https://github.com/clshortfuse/renodx",
        Source::GitHubLatest {
            repo: "clshortfuse/renodx".to_owned(),
            asset_suffix: ".addon64".to_owned(),
        },
    ));

    // The neural *consumers*. Exactly one may be installed: if a second is
    // found loaded beside it, the first does nothing for the entire session,
    // silently. That is the single worst failure mode in this ecosystem and
    // the reason `conflicts_with` exists.
    let mut chicken = component(
        "deep-fried-chicken",
        "Deep Fried Chicken",
        "Performs the neural rendering. The recommended consumer - Feeder builds the request, this answers it.",
        Role::Addon,
        other(
            "not published",
            false,
            "distributed through its author's Discord rather than a release page, so there is nothing to download automatically",
        ),
        "https://discord.gg/g2v2XGqvR",
        Source::UserObtained {
            from: "its author's Discord - take 1.4.8 or newer".to_owned(),
            files: [
                "deep-fried-chicken.addon64",
                "deep-fried-chicken-nvngx.dll",
                "deep-fried-chicken.cfg",
            ]
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        },
    );
    chicken.requires = vec!["reshade".to_owned(), "nvngx-dlssnr".to_owned()];
    components.push(chicken);

    let mut krish = component(
        "renodx-dlss5",
        "RenoDX DLSS 5 add-on",
        "The older neural consumer, still fully supported. An alternative to Deep Fried Chicken - never both.",
        Role::Addon,
        other(
            "not published",
            false,
            "distributed through the RenoDX Discord #DLSS5 channel rather than a release page",
        ),
        "https://discord.com/invite/renodx",
        Source::UserObtained {
            from: "the RenoDX Discord, #DLSS5 channel".to_owned(),
            files: vec!["renodx-dlss5.addon64".to_owned()],
        },
    );
    krish.requires = vec!["reshade".to_owned(), "nvngx-dlssnr".to_owned()];
    components.push(krish);

    // ------------------------------------------------- hardware-gate unlocks
    let mut mfg = component(
        "rtx40-mfg-unlock",
        "RTX 40 Multi Frame Generation Unlock",
        "Enables the higher frame-generation multipliers on RTX 40-series cards. Patches the running game in memory and changes no files on disk.",
        Role::AsiPlugin,
        Licence::Mit,
        "https://github.com/dashdogy/RTX40MFG-Unlock",
        Source::GitHubLatest {
            repo: "dashdogy/RTX40MFG-Unlock".to_owned(),
            asset_suffix: ".zip".to_owned(),
        },
    );
    // Its own README is explicit that a game's Streamline and NVIDIA DLLs must
    // not be replaced or bundled - the unlock works by hooking the loaded
    // process instead. That is the approach worth following for any gated
    // feature, and it is why this needs a loader rather than a swapped DLL.
    mfg.requires = vec!["ultimate-asi-loader".to_owned(), "reshade".to_owned()];
    mfg.experimental = true;
    components.push(mfg);

    components.push(component(
        "ultimate-asi-loader",
        "Ultimate ASI Loader",
        "Loads ASI plugins into a game. Required by the frame-generation unlock.",
        Role::Loader,
        Licence::Mit,
        "https://github.com/ThirteenAG/Ultimate-ASI-Loader",
        Source::GitHubLatest {
            repo: "ThirteenAG/Ultimate-ASI-Loader".to_owned(),
            asset_suffix: ".zip".to_owned(),
        },
    ));

    // ------------------------------------------------------------- upscalers
    //
    // This one carries a whole route: `Route::OptiScaler` takes over the
    // upscaler a game already calls and runs the neural pass over its output.
    // It is the only route here that needs no ReShade, and the only one that
    // gets real upscaling rather than DLAA, because the game it hooks is
    // already jittering its sampling.
    //
    // GPL-3.0, so it is fetched from its own release page and never bundled -
    // not a formality, since bundling it would put this application's own
    // distribution under terms it is not published under.
    components.push(component(
        "optiscaler",
        "OptiScaler (neural rendering build)",
        "Takes over a game's own DLSS, FSR 2/3 or XeSS calls and runs neural rendering over the \
         result - real upscaling, and no ReShade. DirectX 11 and 12, 64-bit.",
        Role::Compat,
        Licence::Gpl3,
        "https://github.com/Dagherbou/OptiScaler_DLSSNR",
        Source::GitHubLatest {
            repo: "Dagherbou/OptiScaler_DLSSNR".to_owned(),
            asset_suffix: ".zip".to_owned(),
        },
    ));

    // -------------------------------------------------------------- shaders
    components.push(component(
        "lumenite",
        "LumeniteFX",
        "Optical-flow motion vectors, which the Feeder route needs in games that provide none.",
        Role::Shaders,
        // Custom "AGNYA License": all rights reserved unless explicitly
        // stated. Fetched from the author only, never shipped or mirrored.
        other(
            "AGNYA License",
            false,
            "reserves all rights unless explicitly granted, so it is fetched from the author rather than shipped with NeuralSwap",
        ),
        "https://github.com/umar-afzaal/LumeniteFX",
        Source::GitHubBranch {
            repo: "umar-afzaal/LumeniteFX".to_owned(),
            branch: "main".to_owned(),
        },
    ));

    components.push(component(
        "vort-shaders",
        "VORT Shaders",
        "Block-matching motion estimation. An alternative motion-vector source for the Feeder route.",
        Role::Shaders,
        Licence::Mit,
        "https://github.com/vortigern11/vort_Shaders",
        Source::GitHubBranch {
            repo: "vortigern11/vort_Shaders".to_owned(),
            branch: "main".to_owned(),
        },
    ));

    // ------------------------------------------------------------ old games
    components.push(component(
        "dgvoodoo2",
        "dgVoodoo 2",
        "Translates DirectX 8 and 9 to DirectX 12, so ReShade can attach to much older games.",
        Role::Compat,
        other(
            "dgVoodoo licence",
            false,
            "forbids bundling in general-purpose launchers, so it is downloaded from the author on first use",
        ),
        "https://github.com/dege-diosg/dgVoodoo2",
        Source::GitHubLatest {
            repo: "dege-diosg/dgVoodoo2".to_owned(),
            asset_suffix: ".zip".to_owned(),
        },
    ));

    // ---------------------------------------------------- NVIDIA's own files
    //
    // Streamline is MIT and could legitimately be shipped. The DLSS runtimes
    // cannot: they are licensed for distribution only as part of an
    // application that uses them, and the neural-rendering runtime has no
    // public release at all. So they are found on the user's machine instead
    // of mirrored onto ours, which is the one part of this that every other
    // tool in the space gets wrong.
    components.push(component(
        "streamline",
        "NVIDIA Streamline",
        "The plumbing DLSS features load through. Open source, so NeuralSwap can supply it directly.",
        Role::Runtime,
        Licence::Mit,
        "https://github.com/NVIDIA-RTX/Streamline",
        Source::Bundled {
            rel: "streamline".to_owned(),
        },
    ));

    for (id, name, summary) in [
        (
            "nvngx-dlss",
            "DLSS Super Resolution runtime",
            "The upscaler itself. NeuralSwap finds the copy already on your machine.",
        ),
        (
            "nvngx-dlssd",
            "DLSS Ray Reconstruction runtime",
            "The denoiser. NeuralSwap finds the copy already on your machine.",
        ),
        (
            "nvngx-dlssg",
            "DLSS Frame Generation runtime",
            "Frame generation. NeuralSwap finds the copy already on your machine.",
        ),
        (
            "nvngx-dlssnr",
            "DLSS 5 Neural Rendering runtime",
            "Neural rendering. NeuralSwap finds the copy already on your machine - NVIDIA has not published this one.",
        ),
    ] {
        components.push(component(
            id,
            name,
            summary,
            Role::Runtime,
            other(
                "NVIDIA RTX SDKs licence",
                false,
                "permits distribution only as part of an application that uses it, and not as a stand-alone product, so NeuralSwap never ships or mirrors it",
            ),
            "https://github.com/NVIDIA/DLSS",
            // Two places worth searching, both already on the user's machine.
            // The driver ships `nvngx.dll` and `nvngx_dlssg.dll` under
            // System32\DriverStore\FileRepository\nvhmi.inf_amd64_*, which is
            // a genuine NVIDIA build by definition - no download and nothing
            // redistributed. It does not carry the other three runtimes, so
            // installed games are the other source.
            Source::LocalOnly {
                hint: "your installed games, or the NVIDIA driver's own copy under \
                       System32\\DriverStore"
                    .to_owned(),
            },
        ));
    }

    // The conflicts, applied symmetrically so neither order of installation
    // produces a different answer. Each pair is a documented silent failure:
    //
    // - Two neural consumers: the first "does nothing at all for the whole
    //   session - silently", per Feeder's README.
    // - Feeder and the DX11 bridge: both authors say not to run both for one
    //   game. Feeder does the bridge's job for games with no DLSS; the bridge
    //   is for games that already have it.
    // - OptiScaler alongside the Feeder route: Feeder's install notes say to
    //   turn it off.
    for (left, right) in [
        ("deep-fried-chicken", "renodx-dlss5"),
        ("dlss5-feeder", "dlss5-bridge"),
        ("dlss5-feeder", "optiscaler"),
    ] {
        for (a, b) in [(left, right), (right, left)] {
            if let Some(entry) = components.iter_mut().find(|item| item.id == a) {
                entry.conflicts_with.push(b.to_owned());
            }
        }
    }

    // Feeder needs a host to load it, something to consume what it produces,
    // and a motion-vector provider. The last is a hard dependency rather than
    // a nicety: DLSS requires motion vectors and a game with no DLSS exposes
    // none, so without a provider the route does not degrade - it does not
    // work.
    if let Some(feeder) = components.iter_mut().find(|item| item.id == "dlss5-feeder") {
        feeder.requires = ["reshade", "lumenite", "nvngx-dlssnr"]
            .iter()
            .map(|id| (*id).to_owned())
            .collect();
    }
    if let Some(bridge) = components.iter_mut().find(|item| item.id == "dlss5-bridge") {
        bridge.requires = ["reshade", "nvngx-dlssnr"]
            .iter()
            .map(|id| (*id).to_owned())
            .collect();
    }

    Catalog {
        version: CATALOG_VERSION,
        components: components
            .into_iter()
            .map(|component| (component.id.clone(), component))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_catalogue_is_coherent() {
        default_catalog()
            .validate()
            .expect("the built-in catalogue");
    }

    #[test]
    fn nvidia_runtimes_are_never_ours_to_ship() {
        // The rule this whole module exists to hold. Every other tool in this
        // space mirrors these; encoding it here means somebody has to argue
        // with a test rather than quietly edit a URL.
        let catalog = default_catalog();
        for id in ["nvngx-dlss", "nvngx-dlssd", "nvngx-dlssg", "nvngx-dlssnr"] {
            let component = catalog.get(id).expect(id);
            assert!(!component.licence.redistributable(), "{id}");
            assert!(!component.source.we_redistribute(), "{id}");
            assert_eq!(component.source.trust(), Trust::UserSupplied, "{id}");
            assert!(
                matches!(component.source, Source::LocalOnly { .. }),
                "{id} must come from the user's own machine"
            );
        }
    }

    #[test]
    fn a_restricted_licence_cannot_be_paired_with_bundling() {
        let mut catalog = default_catalog();
        let entry = catalog
            .components
            .get_mut("lumenite")
            .expect("lumenite is in the catalogue");
        entry.source = Source::Bundled {
            rel: "lumenite".to_owned(),
        };

        let refused = catalog
            .validate()
            .expect_err("bundling this is not allowed");
        assert_eq!(refused.code, Code::BadRequest);
        assert!(refused.detail.contains("cannot be bundled"));
    }

    #[test]
    fn what_we_may_ship_is_exactly_what_permits_it() {
        let catalog = default_catalog();
        for component in catalog.components.values() {
            if component.source.we_redistribute() {
                assert!(
                    component.licence.redistributable(),
                    "{} is bundled but its licence does not permit it",
                    component.id
                );
            }
        }
        // Streamline is the one we do ship, because MIT lets us.
        let streamline = catalog.get("streamline").expect("streamline");
        assert!(streamline.source.we_redistribute());
        assert_eq!(streamline.licence, Licence::Mit);
    }

    #[test]
    fn every_fetch_is_over_https() {
        let mut catalog = default_catalog();
        catalog
            .components
            .get_mut("reshade")
            .expect("reshade")
            .source = Source::Official {
            template: "http://reshade.me/downloads/ReShade_Setup_{version}_Addon.exe".to_owned(),
            known: vec!["6.8.0".to_owned()],
        };
        let refused = catalog.validate().expect_err("plain HTTP is refused");
        assert!(refused.detail.contains("HTTPS"));
    }

    #[test]
    fn a_pinned_source_must_actually_carry_a_digest() {
        let mut catalog = default_catalog();
        catalog.components.get_mut("renodx").expect("renodx").source = Source::Pinned {
            url: "https://example.invalid/renodx.zip".to_owned(),
            sha256: "not-a-digest".to_owned(),
            size: 1,
        };
        assert!(catalog.validate().is_err());
    }

    #[test]
    fn dependencies_must_exist_and_not_be_circular() {
        let mut catalog = default_catalog();
        catalog
            .components
            .get_mut("renodx")
            .expect("renodx")
            .requires = vec!["no-such-component".to_owned()];
        assert!(catalog.validate().is_err());

        let mut circular = default_catalog();
        circular
            .components
            .get_mut("renodx")
            .expect("renodx")
            .requires = vec!["renodx".to_owned()];
        assert!(circular.validate().is_err());
    }

    #[test]
    fn a_catalogue_from_a_newer_build_is_refused() {
        let mut catalog = default_catalog();
        catalog.version = CATALOG_VERSION + 1;
        assert_eq!(
            catalog.validate().err().map(|error| error.code),
            Some(Code::StateVersionAhead)
        );
    }

    #[test]
    fn the_unlock_declares_what_it_needs() {
        // It hooks the running process, so it needs a loader and ReShade for
        // its menu - not a swapped NVIDIA DLL.
        let catalog = default_catalog();
        let mfg = catalog.get("rtx40-mfg-unlock").expect("the unlock");
        assert!(mfg.requires.contains(&"ultimate-asi-loader".to_owned()));
        assert!(mfg.requires.contains(&"reshade".to_owned()));
        assert!(
            mfg.experimental,
            "it is research software and should say so"
        );
        assert_eq!(mfg.role, Role::AsiPlugin);
    }

    #[test]
    fn the_two_neural_consumers_exclude_each_other() {
        // The worst failure in this ecosystem: install both and the first does
        // nothing for the entire session, silently. Feeder's README leads with
        // it as the first thing to check when something looks broken.
        let catalog = default_catalog();
        let chicken = catalog.get("deep-fried-chicken").expect("chicken");
        let krish = catalog.get("renodx-dlss5").expect("renodx-dlss5");
        assert!(chicken.conflicts_with.contains(&krish.id));
        assert!(krish.conflicts_with.contains(&chicken.id));
    }

    #[test]
    fn the_feeder_and_the_bridge_are_alternatives_not_companions() {
        // Feeder does the bridge's job for a game with no DLSS; the bridge is
        // for one that already has it. Both authors say not to run both.
        let catalog = default_catalog();
        let feeder = catalog.get("dlss5-feeder").expect("feeder");
        assert!(feeder.conflicts_with.contains(&"dlss5-bridge".to_owned()));
        // And OptiScaler has to be off for the Feeder route.
        assert!(feeder.conflicts_with.contains(&"optiscaler".to_owned()));
    }

    #[test]
    fn a_one_sided_conflict_is_refused() {
        // A conflict is a property of a pair. Declared on one side only, the
        // warning a user sees would depend on which they installed second.
        let mut catalog = default_catalog();
        catalog
            .components
            .get_mut("renodx")
            .expect("renodx")
            .conflicts_with = vec!["optiscaler".to_owned()];
        let refused = catalog.validate().expect_err("one-sided");
        assert!(refused.detail.contains("does not say so"));
    }

    #[test]
    fn the_feeder_route_declares_its_hard_dependencies() {
        // A motion-vector provider is not optional: DLSS requires motion
        // vectors, a game with no DLSS exposes none, so without a provider the
        // route does not degrade - it does not work.
        let catalog = default_catalog();
        let feeder = catalog.get("dlss5-feeder").expect("feeder");
        for needed in ["reshade", "lumenite", "nvngx-dlssnr"] {
            assert!(
                feeder.requires.contains(&needed.to_owned()),
                "feeder should require {needed}"
            );
        }
    }

    #[test]
    fn the_discord_distributed_add_ons_say_so_rather_than_pretending() {
        // Neither neural consumer is published anywhere fetchable. Naming the
        // files a user must place is more use than an invented download.
        let catalog = default_catalog();
        for id in ["deep-fried-chicken", "renodx-dlss5"] {
            let entry = catalog.get(id).expect(id);
            match &entry.source {
                Source::UserObtained { from, files } => {
                    assert!(!from.is_empty());
                    assert!(!files.is_empty(), "{id} should name its files");
                }
                other => panic!("{id} should be user-obtained, got {other:?}"),
            }
            assert_eq!(entry.source.trust(), Trust::UserSupplied);
            assert!(!entry.source.we_redistribute());
        }
    }

    #[test]
    fn a_catalogue_round_trips_through_json() {
        // It has to, because a live catalogue arrives as JSON and replaces the
        // built-in one only after validating.
        let catalog = default_catalog();
        let text = serde_json::to_string(&catalog).expect("serialise");
        let revived: Catalog = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(revived, catalog);
        revived.validate().expect("still coherent");
    }

    #[test]
    fn moving_targets_are_marked_as_trust_on_first_use() {
        let catalog = default_catalog();
        for id in ["dlss5-feeder", "lumenite", "reshade"] {
            assert_eq!(
                catalog.get(id).expect(id).source.trust(),
                Trust::FirstUse,
                "{id} is a moving target and cannot be pinned"
            );
        }
    }

    #[test]
    fn keys_and_ids_agree() {
        let mut catalog = default_catalog();
        let mut renamed = catalog.get("renodx").expect("renodx").clone();
        renamed.id = "something-else".to_owned();
        catalog.components.insert("renodx".to_owned(), renamed);
        assert!(catalog.validate().is_err());
    }
}
