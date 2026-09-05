//! Where each component's files actually go, and under what name.
//!
//! [`crate::install::recipe`] decides *what* to install and in what order.
//! This decides *where*, which is a separate problem with its own rules -
//! and getting it wrong produces an install that writes every file
//! successfully and loads none of them.
//!
//! # Injectors do not keep their own name
//!
//! Windows resolves a DLL beside the executable before the system copy, so an
//! injector gets loaded by taking the name of a library the game already
//! imports. ReShade's archive holds exactly one DLL per bitness -
//! `ReShade64.dll` - and the installer renames it on the way in:
//!
//! | API | name it takes |
//! | --- | --- |
//! | Direct3D 10/11/12 | `dxgi.dll` |
//! | Direct3D 9 | `d3d9.dll` |
//! | OpenGL | `opengl32.dll` |
//!
//! Which pair of files applies is decided by the **executable's** bitness, not
//! the host's: a 64-bit DLL does not load in a 32-bit process.
//!
//! # Except on Vulkan, where it is a machine-wide layer
//!
//! Vulkan has no library to impersonate usefully, so ReShade ships layer
//! manifests instead - `ReShade64.json`, verified in the 6.8.0 installer:
//!
//! ```json
//! { "layer": { "name": "VK_LAYER_reshade",
//!              "library_path": ".\\ReShade64.dll",
//!              "disable_environment": { "DISABLE_VK_LAYER_reshade_1": "1" } } }
//! ```
//!
//! Registration is a value under `HKCU\Software\Khronos\Vulkan\ImplicitLayers`
//! naming the manifest's absolute path. **That is per account, not per game.**
//! Every registered implicit layer applies to every Vulkan application, so
//! installing this "into a game" is not what it sounds like - it changes the
//! behaviour of all of them, and the files do not live in the game folder at
//! all.
//!
//! Three things follow, and DLSS5-Swapper handles all three:
//!
//! - It has to be **reference counted**. Undoing one game must not deregister
//!   a layer another game still wants, so the shared directory keeps a list.
//! - A registration **we did not make is left alone**, rather than taken over.
//! - There is **no proxy slot to contend for**, so a Vulkan game can host an
//!   injector even when something else already holds `dxgi.dll`.
//!
//! There is an OpenXR manifest too, `XR_APILAYER_reshade`, on the same
//! pattern. Not wired up here - nothing in the catalogue needs it yet - but it
//! is the reason the manifest name is carried as data rather than assumed.
//!
//! # Two proxies in one folder need two names
//!
//! `rtx40-mfg-unlock` requires both ReShade and Ultimate ASI Loader, and both
//! load by proxy. Its author states the rule directly:
//!
//! > ReShade normally owns `dxgi.dll`, so give Ultimate ASI Loader a different
//! > supported proxy name that the game imports at startup [...] Never install
//! > both loaders under the same proxy filename.
//!
//! And the second half matters as much as the first: the name has to be one
//! **the game actually imports**, because "a Vulkan game may never load
//! `dxgi.dll` or `d3d12.dll`". The import table is already read for route
//! detection, so the loader's name is chosen from it rather than fixed.

use serde::{Deserialize, Serialize};

use crate::components::catalog::{Catalog, Role, Source};
use crate::install::recipe::{Recipe, Step};
use crate::scan::api::Api;

/// How one component's files reach the game folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Delivery {
    /// Copied into a directory, keeping their names.
    Copy {
        /// Relative to the game root. Empty means the root itself.
        dir: String,
    },
    /// One file copied and renamed, to be loaded in place of a system library.
    Proxy {
        dir: String,
        /// The name it takes, e.g. `dxgi.dll`.
        as_name: String,
        /// The file in the component's archive that gets renamed.
        from: String,
    },
    /// Registered as a Vulkan implicit layer rather than proxied.
    ///
    /// **Machine-wide, not per game.** The registry value under
    /// `HKCU\Software\Khronos\Vulkan\ImplicitLayers` names an absolute path to
    /// a manifest, and the Vulkan loader applies every registered implicit
    /// layer to every Vulkan application on the account. So this delivery has
    /// no directory inside the game at all: the files live in one shared
    /// place, and installing it for one game changes the behaviour of all of
    /// them.
    ///
    /// That has two consequences the file-copy deliveries do not have, both
    /// taken from how DLSS5-Swapper handles it:
    ///
    /// 1. **It has to be reference counted.** Uninstalling from one game must
    ///    not deregister the layer while another game still wants it, so the
    ///    shared directory carries a list of the games that asked.
    /// 2. **A registration we did not make is left alone.** If some other
    ///    tool - or the user's own ReShade - already has a layer registered,
    ///    taking it over would silently change a setup we do not own.
    VulkanLayer {
        /// The manifest file name inside the shared directory.
        manifest: String,
        /// The layer's own name, for reporting and for the disable switch.
        layer: String,
        /// The DLL the manifest's `library_path` must point at, which differs
        /// by bitness and is rewritten on the way in.
        library: String,
    },
    /// Nothing to copy: the user has to put the files there themselves.
    ///
    /// Several add-ons in this space are distributed through Discord, with no
    /// URL to fetch and no digest to publish. Saying so, and naming the files
    /// expected and the directory they belong in, is more use than a download
    /// button that cannot work.
    ByHand {
        dir: String,
        /// Where to get it, in words.
        from: String,
        files: Vec<String>,
    },
}

impl Delivery {
    /// The directory this delivery targets, relative to the game root.
    ///
    /// `None` for a Vulkan layer, which is registered machine-wide and writes
    /// nothing into the game at all.
    pub fn dir(&self) -> Option<&str> {
        match self {
            Delivery::Copy { dir } | Delivery::Proxy { dir, .. } | Delivery::ByHand { dir, .. } => {
                Some(dir)
            }
            Delivery::VulkanLayer { .. } => None,
        }
    }

    /// Whether this reaches outside the game folder.
    ///
    /// The one that does is the Vulkan layer, and it matters: an install that
    /// changes machine-wide state has to say so before it runs, and has to be
    /// undone by reference count rather than by deleting files.
    pub fn is_machine_wide(&self) -> bool {
        matches!(self, Delivery::VulkanLayer { .. })
    }

    /// Whether this is work the installer performs, as opposed to an
    /// instruction for the user.
    pub fn is_ours(&self) -> bool {
        !matches!(self, Delivery::ByHand { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    pub component: String,
    pub role: Role,
    pub delivery: Delivery,
    /// Why it goes there, for the confirmation screen.
    pub note: String,
}

/// Names Ultimate ASI Loader can take, best first.
///
/// None of them is a graphics library, so whichever name the injector takes
/// for the game's API, the two cannot collide. RTX40MFG-Unlock's author states
/// the constraint directly:
///
/// > ReShade normally owns `dxgi.dll`, so give Ultimate ASI Loader a different
/// > supported proxy name that the game imports at startup, commonly
/// > `dinput8.dll` or `version.dll`. Never install both loaders under the same
/// > proxy filename.
///
/// The list is ordered rather than a single constant because of the other half
/// of that advice: the name has to be one **the game actually imports**. A
/// proxy the game never loads is a file that sits there doing nothing, and the
/// same author warns that "a Vulkan game may never load `dxgi.dll` or
/// `d3d12.dll`".
const LOADER_PROXIES: [&str; 4] = ["version.dll", "dinput8.dll", "winmm.dll", "dbghelp.dll"];

/// ReShade's own file names, which are what its archive contains.
///
/// Read from the 6.8.0 installer rather than assumed: six entries, being
/// `ReShade64.dll`, `ReShade32.dll`, and a Vulkan and an OpenXR manifest for
/// each bitness. Which pair applies is decided by the executable's bitness,
/// not by the host's - a 32-bit game loads a 32-bit injector or nothing.
const RESHADE_VULKAN_LAYER: &str = "VK_LAYER_reshade";

fn reshade_dll(bitness: u8) -> &'static str {
    if bitness == 32 {
        "ReShade32.dll"
    } else {
        "ReShade64.dll"
    }
}

fn reshade_manifest(bitness: u8) -> &'static str {
    if bitness == 32 {
        "ReShade32.json"
    } else {
        "ReShade64.json"
    }
}

/// What the executable is, for choosing files and proxy names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target<'a> {
    /// 32 or 64. Anything else is treated as 64, which is the overwhelming
    /// majority and the only one the modern routes support anyway.
    pub bitness: u8,
    /// What the executable talks to. `None` when the scan could not tell.
    pub api: Option<Api>,
    /// Lower-cased DLL names from the import table, used to pick a proxy the
    /// game will actually load.
    pub imports: &'a [String],
}

/// Decide where every step of a recipe belongs.
///
/// `install_dir` is the directory the runtime loads from - beside the chosen
/// executable - relative to the game root. `api` is what that executable talks
/// to, which decides both the proxy name and whether a proxy is used at all.
///
/// Pure, like the recipe and the plan above it: a confirmation screen and the
/// install that follows it have to be describing the same thing.
pub fn plan(
    catalog: &Catalog,
    recipe: &Recipe,
    install_dir: &str,
    target: &Target<'_>,
) -> Vec<Placement> {
    let dir = install_dir.trim_end_matches(['/', '\\']).replace('\\', "/");
    let loader = loader_proxy(target);

    recipe
        .steps
        .iter()
        .filter(|step| !step.already_present)
        .filter_map(|step| place(catalog, step, &dir, target, loader))
        .collect()
}

/// The name Ultimate ASI Loader should take for this executable.
///
/// Prefers one the game actually imports, because a proxy the game never
/// loads is a file that sits there doing nothing - and for a Vulkan game that
/// is the normal outcome of picking a DirectX name.
///
/// Falls back to the first candidate when the import table names none of them,
/// which happens whenever it names nothing at all. The note on the placement
/// says which of the two cases applied, so a user chasing a loader that never
/// ran is told where to look.
fn loader_proxy(target: &Target<'_>) -> &'static str {
    let injector = target.api.map(Api::hook_name);
    LOADER_PROXIES
        .into_iter()
        .find(|name| Some(*name) != injector && target.imports.iter().any(|import| import == name))
        .unwrap_or(LOADER_PROXIES[0])
}

fn place(
    catalog: &Catalog,
    step: &Step,
    dir: &str,
    target: &Target<'_>,
    loader: &'static str,
) -> Option<Placement> {
    let api = target.api;
    let component = catalog.get(&step.component)?;

    // A component nobody can fetch is an instruction, whatever its role.
    if let Source::UserObtained { from, files } = &component.source {
        return Some(Placement {
            component: step.component.clone(),
            role: step.role,
            delivery: Delivery::ByHand {
                dir: dir.to_owned(),
                from: from.clone(),
                files: files.clone(),
            },
            note: format!(
                "{} is not downloadable. Put its files in this folder yourself.",
                component.name
            ),
        });
    }

    let delivery = match step.role {
        // The injector, which is the only role whose destination depends on
        // what the game talks to.
        Role::Injector => match api {
            Some(Api::Vulkan) => Delivery::VulkanLayer {
                manifest: reshade_manifest(target.bitness).to_owned(),
                layer: RESHADE_VULKAN_LAYER.to_owned(),
                library: reshade_dll(target.bitness).to_owned(),
            },
            Some(api) => Delivery::Proxy {
                dir: dir.to_owned(),
                as_name: api.hook_name().to_owned(),
                from: reshade_dll(target.bitness).to_owned(),
            },
            // The scan could not say what the executable uses - a game that
            // resolves Direct3D through `LoadLibrary` shows nothing
            // statically. `dxgi.dll` covers Direct3D 10, 11 and 12, which is
            // very nearly everything, so it is the default; it is a default
            // rather than a deduction, and the note says so.
            None => Delivery::Proxy {
                dir: dir.to_owned(),
                as_name: Api::Dxgi.hook_name().to_owned(),
                from: reshade_dll(target.bitness).to_owned(),
            },
        },

        // Also a proxy, and deliberately not the same one. See LOADER_PROXIES.
        Role::Loader => Delivery::Proxy {
            dir: dir.to_owned(),
            as_name: loader.to_owned(),
            from: format!("{}.dll", component.id),
        },

        // ReShade reads its shaders and textures from a fixed subdirectory.
        Role::Shaders => Delivery::Copy {
            dir: join(dir, "reshade-shaders"),
        },

        // Add-ons, ASI plugins, runtimes and compatibility layers all sit
        // beside the executable: the add-on because ReShade looks for it
        // there, the runtime because NGX does.
        Role::Addon | Role::AsiPlugin | Role::Runtime | Role::Compat => Delivery::Copy {
            dir: dir.to_owned(),
        },
    };

    let note = describe(&delivery, component.name.as_str(), api);
    Some(Placement {
        component: step.component.clone(),
        role: step.role,
        delivery,
        note,
    })
}

fn describe(delivery: &Delivery, name: &str, api: Option<Api>) -> String {
    match delivery {
        Delivery::VulkanLayer { layer, .. } => format!(
            "{name} installs as the Vulkan layer {layer}, which is a registry entry rather \
             than a file in this game. Nothing here is displaced by it - but the layer \
             applies to every Vulkan program on this account, not only this game, and it \
             stays until the last game using it is undone."
        ),
        Delivery::Proxy { as_name, from, .. } => match api {
            Some(api) => format!(
                "{name} is loaded by taking the place of {as_name}, which is the library a \
                 {api} game asks for. {from} is renamed on the way in."
            ),
            None => format!(
                "{name} is loaded by taking the place of {as_name}. The scan could not tell \
                 which graphics library this game uses, so this is the common case rather \
                 than a certainty - if the game does not start, that is the setting to change."
            ),
        },
        Delivery::Copy { dir } if dir.ends_with("reshade-shaders") => {
            format!("{name} goes in reshade-shaders/, where ReShade looks for it.")
        }
        Delivery::Copy { .. } => {
            format!("{name} goes beside the executable, which is where it is looked for.")
        }
        Delivery::ByHand { from, .. } => format!("{name} has to come from {from}."),
    }
}

fn join(dir: &str, child: &str) -> String {
    if dir.is_empty() {
        child.to_owned()
    } else {
        format!("{dir}/{child}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::catalog::default_catalog;
    use crate::install::recipe;
    use crate::platform::gpu::Generation;
    use crate::scan::capability::Feature;
    use crate::scan::footprints::Survey;
    use crate::scan::integration::{Integration, Route};

    fn feeder_recipe() -> Recipe {
        recipe::build(
            &default_catalog(),
            Route::Feeder,
            &[Feature::NeuralRendering],
            Integration::None,
            &[],
            &Survey::default(),
            Some(Generation::Blackwell),
        )
    }

    /// A 64-bit target with the given API and no imports worth naming.
    fn target(api: Option<Api>) -> Target<'static> {
        Target {
            bitness: 64,
            api,
            imports: &[],
        }
    }

    fn find<'a>(placed: &'a [Placement], id: &str) -> &'a Placement {
        placed
            .iter()
            .find(|item| item.component == id)
            .unwrap_or_else(|| panic!("{id} was not placed"))
    }

    #[test]
    fn the_injector_takes_the_name_the_game_asks_for() {
        let catalog = default_catalog();
        let built = feeder_recipe();

        for (api, expected) in [
            (Api::Dxgi, "dxgi.dll"),
            (Api::D3d9, "d3d9.dll"),
            (Api::OpenGl, "opengl32.dll"),
        ] {
            let placed = plan(&catalog, &built, "bin/x64", &target(Some(api)));
            let reshade = find(&placed, "reshade");
            assert_eq!(
                reshade.delivery,
                Delivery::Proxy {
                    dir: "bin/x64".to_owned(),
                    as_name: expected.to_owned(),
                    from: reshade_dll(64).to_owned(),
                },
                "{api:?}"
            );
        }
    }

    #[test]
    fn on_vulkan_the_injector_is_a_layer_and_keeps_its_name() {
        // Verified against the shipping 6.8.0 installer, which carries
        // `ReShade64.json` declaring `VK_LAYER_reshade`. Renaming the DLL to
        // `dxgi.dll` for a Vulkan game would install something that never
        // loads.
        let catalog = default_catalog();
        let placed = plan(
            &catalog,
            &feeder_recipe(),
            "bin",
            &target(Some(Api::Vulkan)),
        );
        let reshade = find(&placed, "reshade");

        match &reshade.delivery {
            Delivery::VulkanLayer {
                manifest, layer, ..
            } => {
                assert_eq!(manifest, "ReShade64.json");
                assert_eq!(layer, "VK_LAYER_reshade");
            }
            other => panic!("expected a layer, got {other:?}"),
        }
        assert!(reshade.note.contains("registry"), "{}", reshade.note);
    }

    #[test]
    fn an_unknown_api_defaults_and_says_that_it_is_a_default() {
        // Unity and friends resolve Direct3D at startup, so the import table
        // says nothing. Guessing is unavoidable; presenting the guess as a
        // finding is not.
        let catalog = default_catalog();
        let placed = plan(&catalog, &feeder_recipe(), "", &target(None));
        let reshade = find(&placed, "reshade");

        match &reshade.delivery {
            Delivery::Proxy { as_name, dir, .. } => {
                assert_eq!(as_name, "dxgi.dll");
                assert_eq!(dir, "");
            }
            other => panic!("expected a proxy, got {other:?}"),
        }
        assert!(reshade.note.contains("could not tell"), "{}", reshade.note);
    }

    #[test]
    fn shaders_go_where_reshade_reads_them() {
        let catalog = default_catalog();
        let placed = plan(
            &catalog,
            &feeder_recipe(),
            "bin/x64",
            &target(Some(Api::Dxgi)),
        );
        let lumenite = find(&placed, "lumenite");
        assert_eq!(
            lumenite.delivery,
            Delivery::Copy {
                dir: "bin/x64/reshade-shaders".to_owned()
            }
        );
    }

    #[test]
    fn runtimes_and_addons_sit_beside_the_executable() {
        let catalog = default_catalog();
        let placed = plan(
            &catalog,
            &feeder_recipe(),
            "bin/x64",
            &target(Some(Api::Dxgi)),
        );
        for id in ["nvngx-dlssnr", "dlss5-feeder"] {
            assert_eq!(
                find(&placed, id).delivery,
                Delivery::Copy {
                    dir: "bin/x64".to_owned()
                },
                "{id}"
            );
        }
    }

    #[test]
    fn two_proxies_in_one_recipe_take_different_names() {
        // rtx40-mfg-unlock needs Ultimate ASI Loader *and* ReShade, and both
        // load by taking a library's name. If they picked the same one, one of
        // them would silently not load - the failure this whole area keeps
        // producing.
        //
        // Nothing in the catalogue reaches rtx40-mfg-unlock through a route
        // yet, so the recipe is built by hand. Asserting over a recipe that
        // happens not to contain both would pass without testing anything,
        // which is worse than having no test at all.
        let catalog = default_catalog();
        let step = |component: &str, role: Role| Step {
            component: component.to_owned(),
            role,
            because: "test".to_owned(),
            already_present: false,
        };
        let built = Recipe {
            route: Route::NativeSwap,
            steps: vec![
                step("reshade", Role::Injector),
                step("ultimate-asi-loader", Role::Loader),
                step("rtx40-mfg-unlock", Role::AsiPlugin),
            ],
            delivers: Vec::new(),
            refuses: Vec::new(),
            clashes: Vec::new(),
        };

        let placed = plan(&catalog, &built, "bin", &target(Some(Api::Dxgi)));
        let names: Vec<&str> = placed
            .iter()
            .filter_map(|item| match &item.delivery {
                Delivery::Proxy { as_name, .. } => Some(as_name.as_str()),
                _ => None,
            })
            .collect();

        // Both proxies really are here, so the check below means something.
        assert_eq!(names.len(), 2, "expected two proxies, got {names:?}");
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            names.len(),
            "two proxies share a name: {names:?}"
        );
    }

    #[test]
    fn a_thirty_two_bit_game_gets_the_thirty_two_bit_injector() {
        // A 64-bit DLL in a 32-bit process does not load. The bitness comes
        // from the executable, not from the host.
        let catalog = default_catalog();
        let thirty_two = Target {
            bitness: 32,
            api: Some(Api::D3d9),
            imports: &[],
        };
        let placed = plan(&catalog, &feeder_recipe(), "bin", &thirty_two);
        match &find(&placed, "reshade").delivery {
            Delivery::Proxy { from, as_name, .. } => {
                assert_eq!(from, "ReShade32.dll");
                assert_eq!(as_name, "d3d9.dll");
            }
            other => panic!("expected a proxy, got {other:?}"),
        }

        // And its Vulkan manifest is the 32-bit one, pointing at the 32-bit
        // library.
        let vulkan = Target {
            bitness: 32,
            api: Some(Api::Vulkan),
            imports: &[],
        };
        match &find(&plan(&catalog, &feeder_recipe(), "bin", &vulkan), "reshade").delivery {
            Delivery::VulkanLayer {
                manifest, library, ..
            } => {
                assert_eq!(manifest, "ReShade32.json");
                assert_eq!(library, "ReShade32.dll");
            }
            other => panic!("expected a layer, got {other:?}"),
        }
    }

    #[test]
    fn the_loader_takes_a_name_the_game_actually_imports() {
        // RTX40MFG-Unlock's author: "a Vulkan game may never load `dxgi.dll`
        // or `d3d12.dll`". The same applies to the loader's own name - a
        // proxy the game never loads is a file that sits there doing nothing.
        let imports: Vec<String> = ["vulkan-1.dll", "winmm.dll", "kernel32.dll"]
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        let vulkan_game = Target {
            bitness: 64,
            api: Some(Api::Vulkan),
            imports: &imports,
        };
        // `version.dll` is first in the list, but this game does not import
        // it; `winmm.dll` it does.
        assert_eq!(loader_proxy(&vulkan_game), "winmm.dll");

        // Nothing recognisable imported: fall back rather than fail, because
        // an import table that names nothing is normal.
        let silent = Target {
            bitness: 64,
            api: Some(Api::Dxgi),
            imports: &[],
        };
        assert_eq!(loader_proxy(&silent), LOADER_PROXIES[0]);
    }

    #[test]
    fn the_loader_never_takes_the_injectors_name() {
        // Even if the game imports it. Whichever name the injector needs for
        // the API, the loader must choose another.
        for api in [Api::Dxgi, Api::D3d9, Api::OpenGl] {
            let imports: Vec<String> = LOADER_PROXIES
                .iter()
                .chain([&api.hook_name()])
                .map(|name| (*name).to_owned())
                .collect();
            let target = Target {
                bitness: 64,
                api: Some(api),
                imports: &imports,
            };
            assert_ne!(loader_proxy(&target), api.hook_name(), "{api:?}");
        }
    }

    #[test]
    fn a_vulkan_layer_writes_nothing_into_the_game() {
        // It is a registry entry, machine-wide, and the note has to say so -
        // it changes the behaviour of every Vulkan program on the account,
        // which is not what "install into this game" sounds like.
        let catalog = default_catalog();
        let placed = plan(
            &catalog,
            &feeder_recipe(),
            "bin",
            &target(Some(Api::Vulkan)),
        );
        let reshade = find(&placed, "reshade");

        assert!(reshade.delivery.dir().is_none());
        assert!(reshade.delivery.is_machine_wide());
        assert!(reshade.note.contains("every Vulkan"), "{}", reshade.note);
        assert!(reshade.note.contains("last game"), "{}", reshade.note);
    }

    #[test]
    fn a_loader_and_an_injector_never_agree_on_a_name() {
        // The constraint stated directly, independent of whether a recipe
        // happens to contain both today.
        for api in [Api::Dxgi, Api::D3d9, Api::OpenGl, Api::D3d8] {
            assert_ne!(
                api.hook_name(),
                LOADER_PROXIES[0],
                "the loader would collide with an injector on {api:?}"
            );
        }
    }

    #[test]
    fn something_nobody_can_fetch_is_an_instruction_not_a_copy() {
        // Three components in the catalogue are Discord-distributed. A
        // download button that cannot work is worse than a sentence naming
        // the files.
        let catalog = default_catalog();
        let by_hand: Vec<&str> = catalog
            .components
            .values()
            .filter(|component| matches!(component.source, Source::UserObtained { .. }))
            .map(|component| component.id.as_str())
            .collect();
        assert!(!by_hand.is_empty(), "the catalogue has none to test");

        let built = feeder_recipe();
        let placed = plan(&catalog, &built, "bin", &target(Some(Api::Dxgi)));
        for item in &placed {
            if by_hand.contains(&item.component.as_str()) {
                assert!(!item.delivery.is_ours(), "{}", item.component);
                match &item.delivery {
                    Delivery::ByHand { files, from, .. } => {
                        assert!(!files.is_empty(), "{}", item.component);
                        assert!(!from.is_empty(), "{}", item.component);
                    }
                    other => panic!("expected an instruction, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn a_step_already_satisfied_is_not_placed() {
        // Placement is the work list. A dependency the folder already meets is
        // shown by the recipe and does nothing here.
        let catalog = default_catalog();
        let built = feeder_recipe();
        let placed = plan(&catalog, &built, "bin", &target(Some(Api::Dxgi)));
        assert_eq!(
            placed.len(),
            built.to_install().len(),
            "placement and the work list disagree"
        );
    }

    #[test]
    fn every_placement_explains_itself() {
        let catalog = default_catalog();
        for api in [None, Some(Api::Dxgi), Some(Api::Vulkan), Some(Api::D3d9)] {
            for placed in plan(&catalog, &feeder_recipe(), "bin/x64", &target(api)) {
                assert!(!placed.note.is_empty(), "{}", placed.component);
                // Nothing that targets the game folder may point outside it.
                // A Vulkan layer has no directory at all, and is the one
                // delivery allowed to reach beyond the game - loudly.
                match placed.delivery.dir() {
                    Some(dir) => {
                        assert!(!dir.starts_with('/'), "{dir}");
                        assert!(!dir.contains(".."), "{dir}");
                        assert!(!dir.contains(':'), "{dir}");
                        assert!(!placed.delivery.is_machine_wide());
                    }
                    None => assert!(placed.delivery.is_machine_wide()),
                }
            }
        }
    }
}
