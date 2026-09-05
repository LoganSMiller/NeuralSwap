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
//! # Except on Vulkan, where it is a layer
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
//! So on Vulkan the DLL **keeps its own name** and a registry value points the
//! loader at the manifest beside it. Two consequences worth stating: the
//! install shape is different (a registry write, not just files), and there is
//! no proxy slot to contend for, so a Vulkan game can host an injector even
//! when something else already holds `dxgi.dll`.
//!
//! There is an OpenXR manifest too, `XR_APILAYER_reshade`, on the same
//! pattern. Not wired up here - nothing in the catalogue needs it yet - but it
//! is the reason the manifest name is carried as data rather than assumed.
//!
//! # Two proxies in one folder need two names
//!
//! `rtx40-mfg-unlock` requires both ReShade and Ultimate ASI Loader, and both
//! load by proxy. They cannot both be `dxgi.dll`. The injector takes the
//! graphics name because that is the one it has to intercept; the loader takes
//! a name nothing graphical wants.

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
    /// The files keep their names; a registry value under
    /// `SOFTWARE\Khronos\Vulkan\ImplicitLayers` names the manifest.
    VulkanLayer {
        dir: String,
        /// The manifest the registry value points at.
        manifest: String,
        /// The layer's own name, for reporting and for the disable switch.
        layer: String,
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
    pub fn dir(&self) -> &str {
        match self {
            Delivery::Copy { dir }
            | Delivery::Proxy { dir, .. }
            | Delivery::VulkanLayer { dir, .. }
            | Delivery::ByHand { dir, .. } => dir,
        }
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

/// The name Ultimate ASI Loader takes when ReShade is also being installed.
///
/// The loader supports several - `dinput8.dll`, `winmm.dll`, `dxgi.dll` and
/// others - and `version.dll` is chosen precisely because nothing graphical
/// wants it. Whichever the injector takes for the game's API, the two cannot
/// collide.
const LOADER_PROXY: &str = "version.dll";

/// ReShade's own file names, which are what its archive contains.
///
/// Read from the 6.8.0 installer rather than assumed: six entries, being
/// `ReShade64.dll`, `ReShade32.dll`, and a Vulkan and an OpenXR manifest for
/// each bitness.
const RESHADE_DLL: &str = "ReShade64.dll";
const RESHADE_VULKAN_MANIFEST: &str = "ReShade64.json";
const RESHADE_VULKAN_LAYER: &str = "VK_LAYER_reshade";

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
    api: Option<Api>,
) -> Vec<Placement> {
    let dir = install_dir.trim_end_matches(['/', '\\']).replace('\\', "/");

    recipe
        .steps
        .iter()
        .filter(|step| !step.already_present)
        .filter_map(|step| place(catalog, step, &dir, api))
        .collect()
}

fn place(catalog: &Catalog, step: &Step, dir: &str, api: Option<Api>) -> Option<Placement> {
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
                dir: dir.to_owned(),
                manifest: RESHADE_VULKAN_MANIFEST.to_owned(),
                layer: RESHADE_VULKAN_LAYER.to_owned(),
            },
            Some(api) => Delivery::Proxy {
                dir: dir.to_owned(),
                as_name: api.hook_name().to_owned(),
                from: RESHADE_DLL.to_owned(),
            },
            // The scan could not say what the executable uses - a game that
            // resolves Direct3D through `LoadLibrary` shows nothing
            // statically. `dxgi.dll` covers Direct3D 10, 11 and 12, which is
            // very nearly everything, so it is the default; it is a default
            // rather than a deduction, and the note says so.
            None => Delivery::Proxy {
                dir: dir.to_owned(),
                as_name: Api::Dxgi.hook_name().to_owned(),
                from: RESHADE_DLL.to_owned(),
            },
        },

        // Also a proxy, and deliberately not the same one. See LOADER_PROXY.
        Role::Loader => Delivery::Proxy {
            dir: dir.to_owned(),
            as_name: LOADER_PROXY.to_owned(),
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
            "{name} installs as the Vulkan layer {layer} rather than replacing a library, so \
             it keeps its own name and is enabled through the registry. Nothing else in the \
             folder can be displaced by it."
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
            let placed = plan(&catalog, &built, "bin/x64", Some(api));
            let reshade = find(&placed, "reshade");
            assert_eq!(
                reshade.delivery,
                Delivery::Proxy {
                    dir: "bin/x64".to_owned(),
                    as_name: expected.to_owned(),
                    from: RESHADE_DLL.to_owned(),
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
        let placed = plan(&catalog, &feeder_recipe(), "bin", Some(Api::Vulkan));
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
        let placed = plan(&catalog, &feeder_recipe(), "", None);
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
        let placed = plan(&catalog, &feeder_recipe(), "bin/x64", Some(Api::Dxgi));
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
        let placed = plan(&catalog, &feeder_recipe(), "bin/x64", Some(Api::Dxgi));
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

        let placed = plan(&catalog, &built, "bin", Some(Api::Dxgi));
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
    fn a_loader_and_an_injector_never_agree_on_a_name() {
        // The constraint stated directly, independent of whether a recipe
        // happens to contain both today.
        for api in [Api::Dxgi, Api::D3d9, Api::OpenGl, Api::D3d8] {
            assert_ne!(
                api.hook_name(),
                LOADER_PROXY,
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
        let placed = plan(&catalog, &built, "bin", Some(Api::Dxgi));
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
        let placed = plan(&catalog, &built, "bin", Some(Api::Dxgi));
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
            for placed in plan(&catalog, &feeder_recipe(), "bin/x64", api) {
                assert!(!placed.note.is_empty(), "{}", placed.component);
                // Nothing may be placed outside the game folder.
                let dir = placed.delivery.dir();
                assert!(!dir.starts_with('/'), "{dir}");
                assert!(!dir.contains(".."), "{dir}");
                assert!(!dir.contains(':'), "{dir}");
            }
        }
    }
}
