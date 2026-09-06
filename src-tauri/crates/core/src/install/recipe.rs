//! Turning "this game could have neural rendering" into "install these, in
//! this order".
//!
//! [`crate::scan::capability`] answers what a route could deliver and how good
//! it would look. That is a verdict, and a verdict is not an install. This
//! module answers the other half: which components, in which order, and what
//! has to be true first.
//!
//! # Why this is not just a list per route
//!
//! The community stack is a dependency graph with silent failure modes, and
//! every one of them is a real report from a real user:
//!
//! - The Feeder needs a motion-vector shader pack. DLSS requires
//!   `kBufferTypeMotionVectors`, a game that never had DLSS has no reason to
//!   produce any, and without them the route does not degrade - it does not
//!   work. So `lumenite` is a hard requirement rather than an enhancement.
//! - Two neural consumers beside each other and the first one does nothing,
//!   for the whole session, with no error anywhere.
//! - OptiScaler left enabled breaks the Feeder route.
//! - The Feeder and the DX11 bridge together are warned against by both
//!   authors.
//!
//! None of those announce themselves, so none of them can be left to the user
//! to remember. They are edges in [`crate::components::catalog`], and this
//! module is what reads them.
//!
//! # What it refuses to do
//!
//! A recipe that cannot deliver a feature says so instead of installing
//! something that will not help. Frame generation on the Feeder route is the
//! clearest case: it needs Reflex latency markers, which are a protocol the
//! game takes part in rather than a buffer anything can hand over, so no
//! arrangement of files produces it. Installing the frame generation runtime
//! anyway would leave a user with a folder full of files and no frames.

use serde::{Deserialize, Serialize};

use crate::components::catalog::{Catalog, Role};
use crate::platform::gpu::Generation;
use crate::scan::capability::{outlook, Feature, Quality};
use crate::scan::footprints::{Survey, Tool};
use crate::scan::integration::{Integration, Route};

/// One component to install, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub component: String,
    pub role: Role,
    /// Why this is here, in terms a user can check. Either the feature that
    /// asked for it, or the component that depends on it.
    pub because: String,
    /// Already in the game folder, so this step is a requirement that is
    /// satisfied rather than work to do.
    ///
    /// This matters most for the injector. A user with a working ReShade has
    /// a `reshade.ini`, a shader collection and very possibly other add-ons
    /// beside it, none of which are ours. Reinstalling over that to satisfy a
    /// dependency it already satisfies is the kind of helpfulness that loses
    /// somebody an afternoon of tuning, so the step is kept - the dependency
    /// is real and worth showing - and marked as met.
    pub already_present: bool,
}

/// A feature this recipe will actually produce, and how well.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivers {
    pub feature: Feature,
    pub quality: Quality,
    pub note: String,
}

/// A feature asked for that this recipe will not produce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Refuses {
    pub feature: Feature,
    /// Said plainly. A user who is told "not supported" will try again; one
    /// who is told why will not.
    pub reason: String,
}

/// Something already in the game folder that stops this recipe working.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clash {
    pub tool: Tool,
    /// The component in this recipe it conflicts with.
    pub with: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recipe {
    pub route: Route,
    /// Components in dependency order: nothing appears before something it
    /// needs.
    pub steps: Vec<Step>,
    pub delivers: Vec<Delivers>,
    pub refuses: Vec<Refuses>,
    /// Conflicts with what is already installed. Not fatal on their own - the
    /// user may be about to remove them - but an install over one of these is
    /// the silent-failure case this whole module exists for.
    pub clashes: Vec<Clash>,
}

impl Recipe {
    /// Whether this recipe does anything at all.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Whether it can be run as it stands.
    ///
    /// A clash is not a refusal - it is something to clear first - so it is
    /// reported separately and checked here rather than folded into
    /// [`Recipe::refuses`].
    pub fn is_runnable(&self) -> bool {
        !self.steps.is_empty() && self.clashes.is_empty() && !self.delivers.is_empty()
    }

    /// The steps that are actually work, in order.
    ///
    /// What the installer runs. A dependency the folder already satisfies is
    /// shown to the user and skipped here, so an existing ReShade is not
    /// reinstalled over.
    pub fn to_install(&self) -> Vec<&Step> {
        self.steps
            .iter()
            .filter(|step| !step.already_present)
            .collect()
    }
}

/// What a route needs installed before any feature can work.
///
/// The runtimes are added per feature; this is the machinery that carries
/// them. `None` means the route needs no consumer - a native swap replaces
/// files the game already loads.
fn consumer_for(route: Route) -> Option<&'static str> {
    match route {
        Route::NativeSwap => None,
        Route::Bridge => Some("dlss5-bridge"),
        Route::Feeder => Some("dlss5-feeder"),
        // The proxy DLL, its ini and the neural pass arrive as one component,
        // fetched from its own release page rather than bundled: OptiScaler is
        // GPL-3.0 and this application is not.
        Route::OptiScaler => Some("optiscaler"),
    }
}

/// What to try instead, when a route cannot deliver a feature.
///
/// Pointing at the next route is the difference between a refusal a user can
/// act on and a dead end. The fed routes manufacture the inputs the game will
/// not produce, which is precisely the case being refused here.
fn route_suggestion(route: Route) -> &'static str {
    match route {
        Route::NativeSwap => {
            "The bridge or feeder route can manufacture those inputs instead, at lower quality."
        }
        Route::Bridge | Route::Feeder => {
            "This route already manufactures what it can, so this feature is out of reach here."
        }
        // Not "out of reach here" - this route synthesises nothing, so a
        // missing input is missing because the game never produced it, and
        // the routes that invent inputs are still worth naming.
        Route::OptiScaler => {
            "This route uses the game's own inputs rather than inventing any, so the feeder              route is what remains - at lower quality."
        }
    }
}

/// The catalogue id of the runtime that implements a feature.
fn runtime_for(feature: Feature) -> &'static str {
    match feature {
        Feature::SuperResolution => "nvngx-dlss",
        Feature::RayReconstruction => "nvngx-dlssd",
        Feature::FrameGeneration => "nvngx-dlssg",
        Feature::NeuralRendering => "nvngx-dlssnr",
    }
}

/// Build the install recipe for one route and one set of wanted features.
///
/// Pure: everything it knows arrives as an argument, so the same inputs give
/// the same recipe and it can be tested without a game folder. The same
/// reasoning that keeps [`outlook`] pure - a planner that reads the filesystem
/// is a planner nobody can reproduce.
pub fn build(
    catalog: &Catalog,
    route: Route,
    wanted: &[Feature],
    integration: Integration,
    game_feeds: &[Feature],
    survey: &Survey,
    card: Option<Generation>,
) -> Recipe {
    let mut delivers = Vec::new();
    let mut refuses = Vec::new();
    let mut roots: Vec<(String, String)> = Vec::new();

    for &feature in wanted {
        let found = outlook(feature, integration, route, game_feeds, card);
        match found.quality {
            // Nothing installed here will make the card able to run it, and
            // nothing installed here will conjure an input the renderer never
            // produces.
            Quality::HardwareTooOld | Quality::OutOfReach => {
                refuses.push(Refuses {
                    feature,
                    reason: found.note,
                });
                continue;
            }
            Quality::Native | Quality::Mirrored | Quality::Estimated => {}
        }

        // A feature the game does not itself request needs something to ask
        // on its behalf. On a fed route the route's own consumer already
        // does - adding a second one is the failure this module's
        // documentation opens with, two neural consumers beside each other
        // and the first does nothing for the whole session. On a native swap
        // there is nothing else in the install, so the question is whether an
        // add-on exists that can do the asking.
        //
        // For neural rendering one does: hooking the DLSS a game already
        // invokes and requesting feature 1004 is exactly what the consumer
        // add-ons in this space are for.
        //
        // For anything else, one does not, and this used to pretend
        // otherwise. A Streamline game shipping `sl.dlss` but not
        // `sl.dlss_d` reads as "does not request ray reconstruction", and the
        // recipe answered by installing a *neural* consumer and attributing it
        // to ray reconstruction - which it cannot deliver. Ray reconstruction
        // needs the game to call `slDLSSDSetOptions` and tag albedo, normals
        // and roughness; no add-on can do that on the game's behalf.
        if found.needs_consumer_addon.is_some() && consumer_for(route).is_none() {
            match feature {
                Feature::NeuralRendering => {
                    if let Some(addon) = neural_consumer(catalog) {
                        roots.push((
                            addon.to_owned(),
                            "Neural Rendering has to be requested by an add-on".to_owned(),
                        ));
                    }
                }
                other => {
                    let (plugin, id) = other.streamline_plugin();
                    refuses.push(Refuses {
                        feature: other,
                        reason: format!(
                            "This game does not ship {plugin}, so it never asks Streamline for \
                             {} (feature {id}), and no add-on can ask on its behalf - the game \
                             itself has to tag the inputs. {}",
                            other.label(),
                            route_suggestion(route)
                        ),
                    });
                    continue;
                }
            }
        }

        roots.push((
            runtime_for(feature).to_owned(),
            format!("{} needs its runtime", feature.label()),
        ));

        delivers.push(Delivers {
            feature,
            quality: found.quality,
            note: found.note,
        });
    }

    // The route's own machinery, but only if it is going to carry something.
    // A recipe that installs ReShade and the Feeder to deliver nothing is a
    // folder full of files and no benefit.
    if !delivers.is_empty() {
        if let Some(consumer) = consumer_for(route) {
            roots.push((
                consumer.to_owned(),
                format!("the {} route runs through it", route.label()),
            ));
        }
    }

    let steps = resolve(catalog, &roots, survey);
    let clashes = clashes_with_disk(catalog, &steps, survey);

    Recipe {
        route,
        steps,
        delivers,
        refuses,
        clashes,
    }
}

/// The neural consumer add-on to install.
///
/// There are two - `renodx-dlss5` and `deep-fried-chicken` - and they are
/// mutually exclusive: two neural consumers beside each other and the first
/// one does nothing for the whole session. So exactly one is chosen.
///
/// It would be better to choose the one the user already has, and that is
/// deliberately *not* attempted. Neither has a footprint this scanner can tell
/// apart: `Tool::RenoDx` matches the base RenoDX add-on, which is a different
/// component from its DLSS 5 variant, and `deep-fried-chicken` has no
/// signature at all. Guessing from `Tool::RenoDx` would recommend the DLSS
/// variant to somebody who installed plain RenoDX for tone mapping - a
/// confident wrong answer of exactly the kind this project keeps finding.
///
/// So one is picked and named. Both are Discord-distributed and neither can be
/// fetched, so this step is an instruction either way - and an instruction
/// naming the wrong file is easy for a user to correct, where a
/// silently-ignored second consumer is not.
fn neural_consumer(catalog: &Catalog) -> Option<&'static str> {
    ["renodx-dlss5", "deep-fried-chicken"]
        .into_iter()
        .find(|id| catalog.get(id).is_some())
}

/// Close over `requires` and return the components in dependency order.
///
/// Kahn's algorithm rather than a role table. Ordering by role would be a
/// second place for the truth to live, and it would be wrong the first time a
/// component needed something outside its own role - `rtx40-mfg-unlock` is an
/// ASI plugin that needs both a loader and the injector.
///
/// Ties are broken alphabetically so the same inputs always produce the same
/// list, which is what makes a recipe something a user can be shown before
/// they agree to it.
fn resolve(catalog: &Catalog, roots: &[(String, String)], survey: &Survey) -> Vec<Step> {
    use std::collections::{BTreeMap, BTreeSet};

    // Why each component is here. The first reason wins: a component pulled
    // in directly by a feature is better explained by that feature than by
    // whatever else also happens to need it.
    let mut because: BTreeMap<String, String> = BTreeMap::new();
    let mut wanted: BTreeSet<String> = BTreeSet::new();
    let mut queue: Vec<String> = Vec::new();

    for (id, reason) in roots {
        if catalog.get(id).is_none() {
            continue;
        }
        because.entry(id.clone()).or_insert_with(|| reason.clone());
        if wanted.insert(id.clone()) {
            queue.push(id.clone());
        }
    }

    while let Some(id) = queue.pop() {
        let Some(component) = catalog.get(&id) else {
            continue;
        };
        for needed in &component.requires {
            if catalog.get(needed).is_none() {
                continue;
            }
            because
                .entry(needed.clone())
                .or_insert_with(|| format!("{} needs it", component.name));
            if wanted.insert(needed.clone()) {
                queue.push(needed.clone());
            }
        }
    }

    // What the folder already has, in catalogue terms - and only what is
    // actually going to load.
    //
    // Presence is not enough for an injector. A folder can hold every file
    // ReShade ever wrote and still not load it, because something else took
    // the proxy slot; Ready or Not on the development machine is exactly
    // that. Marking those leftovers as a satisfied dependency would skip the
    // injector and produce an install where the add-on never loads.
    let present: BTreeSet<&str> = survey
        .tools
        .iter()
        .map(|found| found.tool)
        .filter(|tool| survey.is_loading(*tool))
        .filter_map(Tool::component_id)
        .collect();

    // Kahn, over the sub-graph we actually selected.
    let mut remaining: BTreeSet<String> = wanted.clone();
    let mut ordered: Vec<Step> = Vec::new();
    while !remaining.is_empty() {
        // Everything whose dependencies are already placed, alphabetically.
        let ready: Vec<String> = remaining
            .iter()
            .filter(|id| {
                catalog.get(id).is_some_and(|component| {
                    component
                        .requires
                        .iter()
                        .all(|needed| !remaining.contains(needed))
                })
            })
            .cloned()
            .collect();

        if ready.is_empty() {
            // A cycle. The catalogue validator refuses one, so reaching here
            // means the catalogue changed without the validator running -
            // emit the rest in a stable order rather than looping forever.
            for id in remaining.iter() {
                if let Some(component) = catalog.get(id) {
                    ordered.push(Step {
                        component: id.clone(),
                        role: component.role,
                        because: because.get(id).cloned().unwrap_or_default(),
                        already_present: present.contains(&id.as_str()),
                    });
                }
            }
            break;
        }

        for id in ready {
            if let Some(component) = catalog.get(&id) {
                ordered.push(Step {
                    component: id.clone(),
                    role: component.role,
                    because: because.get(&id).cloned().unwrap_or_default(),
                    already_present: present.contains(&id.as_str()),
                });
            }
            remaining.remove(&id);
        }
    }
    ordered
}

/// Conflicts between this recipe and what is already in the game folder.
fn clashes_with_disk(catalog: &Catalog, steps: &[Step], survey: &Survey) -> Vec<Clash> {
    let mut found = Vec::new();
    for step in steps {
        let Some(component) = catalog.get(&step.component) else {
            continue;
        };
        for conflict in &component.conflicts_with {
            for present in &survey.tools {
                if present.tool.component_id() != Some(conflict.as_str()) {
                    continue;
                }
                let name = catalog
                    .get(conflict)
                    .map_or(conflict.as_str(), |other| other.name.as_str());
                found.push(Clash {
                    tool: present.tool,
                    with: step.component.clone(),
                    reason: format!(
                        "{name} is already installed here and cannot run alongside {}. The \
                         failure is silent - one of them simply does nothing for the whole \
                         session - so it has to be removed first.",
                        component.name
                    ),
                });
            }
        }
    }
    // The proxy slot, which is a conflict the catalogue cannot express.
    //
    // There is one `dxgi.dll` per folder. Two injectors both want to be it,
    // and the second either loses or overwrites the first - so an install
    // that adds an injector while a different one holds the slot is a clash
    // however few edges the catalogue draws between them. ReShade and
    // OptiScaler have no `conflicts_with` entry against each other, because
    // in principle they coexist; in one folder, contending for one filename,
    // they do not.
    if let Some(slot) = &survey.proxy {
        if let Some(owner) = slot.owner {
            for step in steps {
                if step.already_present {
                    continue;
                }
                let Some(component) = catalog.get(&step.component) else {
                    continue;
                };
                // Only an injector contends for the slot, and only a
                // *different* one is a problem.
                let ours_injects = matches!(component.role, Role::Injector | Role::Loader)
                    || component.id == "optiscaler";
                if !ours_injects || owner.component_id() == Some(component.id.as_str()) {
                    continue;
                }
                let name = catalog
                    .get(owner.component_id().unwrap_or_default())
                    .map_or_else(|| format!("{owner:?}"), |other| other.name.clone());
                found.push(Clash {
                    tool: owner,
                    with: step.component.clone(),
                    reason: format!(
                        "{name} already holds {} in this folder, and there is only one of it. \
                         Installing {} would take that name over, so one of the two would stop \
                         loading - with no error either way.",
                        slot.file, component.name
                    ),
                });
            }
        }
    }

    found.sort_by(|left, right| {
        (left.with.as_str(), left.tool as u8).cmp(&(right.with.as_str(), right.tool as u8))
    });
    found.dedup_by(|left, right| left.with == right.with && left.tool == right.tool);
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::catalog::default_catalog;
    use crate::scan::footprints::Footprint;

    fn nothing() -> Survey {
        Survey {
            tools: Vec::new(),
            displaced: Vec::new(),
            proxy: None,
        }
    }

    /// A tool that is present but not loading: its files are there, something
    /// else holds the proxy slot.
    fn leftovers_of(tool: Tool) -> Survey {
        Survey {
            tools: vec![Footprint {
                tool,
                evidence: "test".to_owned(),
            }],
            displaced: Vec::new(),
            proxy: None,
        }
    }

    /// A tool that is present *and* holds the loading slot.
    fn loading(tool: Tool) -> Survey {
        Survey {
            tools: vec![Footprint {
                tool,
                evidence: "test".to_owned(),
            }],
            displaced: Vec::new(),
            proxy: Some(crate::scan::footprints::ProxySlot {
                file: "dxgi.dll".to_owned(),
                owner: Some(tool),
                // Whatever is in the slot is assumed usable here; the case
                // where it is not has its own test.
                addon_capable: true,
            }),
        }
    }

    fn position(recipe: &Recipe, id: &str) -> Option<usize> {
        recipe.steps.iter().position(|step| step.component == id)
    }

    #[test]
    fn a_game_with_no_dlss_gets_the_whole_feeder_stack_in_dependency_order() {
        // The headline case: a game that never had DLSS. Everything has to be
        // manufactured, so everything has to be installed.
        let catalog = default_catalog();
        let recipe = build(
            &catalog,
            Route::Feeder,
            &[Feature::NeuralRendering],
            Integration::None,
            &[],
            &nothing(),
            Some(Generation::Blackwell),
        );

        assert!(recipe.is_runnable(), "{recipe:#?}");
        let feeder = position(&recipe, "dlss5-feeder").expect("the consumer");
        let reshade = position(&recipe, "reshade").expect("the injector");
        let lumenite = position(&recipe, "lumenite").expect("motion vectors");
        let runtime = position(&recipe, "nvngx-dlssnr").expect("the runtime");

        // Nothing may appear before something it needs. ReShade has to exist
        // before an add-on is dropped beside it, and the motion-vector pack
        // is not optional - without it the route does not degrade, it fails.
        assert!(reshade < feeder, "{recipe:#?}");
        assert!(lumenite < feeder, "{recipe:#?}");
        assert!(runtime < feeder, "{recipe:#?}");
    }

    #[test]
    fn a_fed_route_never_installs_a_second_neural_consumer() {
        // Found by running this against a real library. On the Feeder route
        // the game feeds nothing, so every feature reads as "the game does
        // not request this" - and an earlier version answered that by adding
        // a neural consumer on top of the Feeder, which is itself the
        // consumer. Two of them beside each other and the first does nothing
        // for the whole session, silently.
        let catalog = default_catalog();
        for route in [Route::Feeder, Route::Bridge] {
            let recipe = build(
                &catalog,
                route,
                &Feature::ALL,
                Integration::None,
                &[],
                &nothing(),
                Some(Generation::Blackwell),
            );

            let consumers: Vec<&str> = recipe
                .steps
                .iter()
                .map(|step| step.component.as_str())
                .filter(|id| {
                    [
                        "dlss5-feeder",
                        "dlss5-bridge",
                        "renodx-dlss5",
                        "deep-fried-chicken",
                    ]
                    .contains(id)
                })
                .collect();
            assert_eq!(consumers.len(), 1, "{route:?} installs {consumers:?}");
        }
    }

    #[test]
    fn frame_generation_is_refused_on_the_feeder_rather_than_half_installed() {
        // Reflex is a protocol the game takes part in, not a buffer anything
        // can hand over. Installing the runtime would leave a user with files
        // and no frames, so the recipe says no instead.
        let catalog = default_catalog();
        let recipe = build(
            &catalog,
            Route::Feeder,
            &[Feature::FrameGeneration],
            Integration::None,
            &[],
            &nothing(),
            Some(Generation::Blackwell),
        );

        assert!(recipe.delivers.is_empty(), "{recipe:#?}");
        let refused = recipe
            .refuses
            .iter()
            .find(|item| item.feature == Feature::FrameGeneration)
            .expect("refused");
        assert!(refused.reason.contains("Reflex"), "{}", refused.reason);

        // And nothing is installed for it. A recipe that delivers nothing must
        // not still drag in ReShade and the Feeder.
        assert!(recipe.steps.is_empty(), "{recipe:#?}");
        assert!(!recipe.is_runnable());
    }

    #[test]
    fn an_existing_reshade_is_a_satisfied_dependency_not_work_to_redo() {
        // A user with a working ReShade has a reshade.ini, a shader
        // collection and probably other add-ons beside it, none of it ours.
        // Reinstalling over that to satisfy a dependency it already satisfies
        // would cost somebody an afternoon of tuning for no gain.
        let catalog = default_catalog();
        let recipe = build(
            &catalog,
            Route::Feeder,
            &[Feature::NeuralRendering],
            Integration::None,
            &[],
            &loading(Tool::ReShade),
            Some(Generation::Blackwell),
        );

        let reshade = recipe
            .steps
            .iter()
            .find(|step| step.component == "reshade")
            .expect("still listed, because the dependency is real");
        assert!(reshade.already_present, "{recipe:#?}");

        // Shown, but not run.
        assert!(!recipe
            .to_install()
            .iter()
            .any(|step| step.component == "reshade"));
        // And the rest of the recipe is still work to do.
        assert!(recipe
            .to_install()
            .iter()
            .any(|step| step.component == "dlss5-feeder"));
        assert!(recipe.is_runnable(), "{recipe:#?}");
    }

    #[test]
    fn leftover_reshade_files_do_not_satisfy_the_injector() {
        // Ready or Not on the development machine: `reshade-shaders/` is
        // there, `OptiScaler.ini` is there, and the single `dxgi.dll` is
        // OptiScaler - 65 mentions of it against 6 of ReShade. ReShade is not
        // loading in that game; what is left of it is a shader folder.
        //
        // Treating that as a satisfied dependency skips the injector, and the
        // add-on that needed it never loads. The install would report success.
        let catalog = default_catalog();
        let recipe = build(
            &catalog,
            Route::Feeder,
            &[Feature::NeuralRendering],
            Integration::None,
            &[],
            &leftovers_of(Tool::ReShade),
            Some(Generation::Blackwell),
        );

        let reshade = recipe
            .steps
            .iter()
            .find(|step| step.component == "reshade")
            .expect("listed");
        assert!(
            !reshade.already_present,
            "leftovers were mistaken for a working install: {recipe:#?}"
        );
        assert!(recipe
            .to_install()
            .iter()
            .any(|step| step.component == "reshade"));
    }

    #[test]
    fn optiscaler_already_installed_is_reported_as_a_clash() {
        // OptiScaler left enabled breaks the Feeder route, and the failure is
        // silent. The user has to be told before the install, not after.
        let catalog = default_catalog();
        let recipe = build(
            &catalog,
            Route::Feeder,
            &[Feature::NeuralRendering],
            Integration::None,
            &[],
            &loading(Tool::OptiScaler),
            Some(Generation::Blackwell),
        );

        let clash = recipe
            .clashes
            .iter()
            .find(|clash| clash.tool == Tool::OptiScaler)
            .expect("the clash");
        assert_eq!(clash.with, "dlss5-feeder");
        assert!(clash.reason.contains("silent"), "{}", clash.reason);
        // Steps are still produced - the user may be about to remove it - but
        // the recipe is not runnable as it stands.
        assert!(!recipe.steps.is_empty());
        assert!(!recipe.is_runnable(), "{recipe:#?}");
    }

    #[test]
    fn two_injectors_contending_for_one_filename_is_a_clash() {
        // The catalogue draws no edge between ReShade and OptiScaler, because
        // in principle they coexist. In one folder, contending for one
        // `dxgi.dll`, they do not - and Ready or Not on the development
        // machine is exactly that: both installed, OptiScaler holding the
        // slot, ReShade inert.
        let catalog = default_catalog();
        let recipe = build(
            &catalog,
            Route::Bridge,
            &[Feature::NeuralRendering],
            Integration::None,
            &[Feature::RayReconstruction],
            &loading(Tool::OptiScaler),
            Some(Generation::Blackwell),
        );

        let clash = recipe
            .clashes
            .iter()
            .find(|clash| clash.with == "reshade")
            .expect("the proxy slot clash");
        assert_eq!(clash.tool, Tool::OptiScaler);
        assert!(clash.reason.contains("dxgi.dll"), "{}", clash.reason);
        assert!(clash.reason.contains("only one"), "{}", clash.reason);
        assert!(!recipe.is_runnable());
    }

    #[test]
    fn the_injector_we_want_already_holding_the_slot_is_not_a_clash() {
        // The same check must not fire against ourselves. A folder where
        // ReShade already owns `dxgi.dll` is the good case, not a conflict.
        let catalog = default_catalog();
        let recipe = build(
            &catalog,
            Route::Feeder,
            &[Feature::NeuralRendering],
            Integration::None,
            &[],
            &loading(Tool::ReShade),
            Some(Generation::Blackwell),
        );

        assert!(recipe.clashes.is_empty(), "{recipe:#?}");
        assert!(recipe.is_runnable(), "{recipe:#?}");
    }

    #[test]
    fn a_native_swap_installs_files_and_not_an_injector() {
        // A game that already feeds the feature needs its runtime replaced and
        // nothing else. Pulling in ReShade here would be overhead for no
        // reason, which is the thing this project is meant to avoid.
        let catalog = default_catalog();
        let recipe = build(
            &catalog,
            Route::NativeSwap,
            &[Feature::SuperResolution],
            Integration::Streamline,
            &[Feature::SuperResolution],
            &nothing(),
            Some(Generation::Blackwell),
        );

        assert_eq!(
            recipe
                .steps
                .iter()
                .map(|step| step.component.as_str())
                .collect::<Vec<_>>(),
            vec!["nvngx-dlss"]
        );
        assert!(recipe.is_runnable());
    }

    #[test]
    fn neural_rendering_on_a_native_swap_still_needs_an_add_on() {
        // The correction from the plugin manifests, now expressed as an
        // install: the game produces everything neural rendering consumes, but
        // it never asks Streamline for feature 1004, so a consumer has to.
        let catalog = default_catalog();
        let recipe = build(
            &catalog,
            Route::NativeSwap,
            &[Feature::NeuralRendering],
            Integration::Streamline,
            &[Feature::RayReconstruction],
            &nothing(),
            Some(Generation::Blackwell),
        );

        assert!(position(&recipe, "nvngx-dlssnr").is_some(), "{recipe:#?}");
        let addon = position(&recipe, "renodx-dlss5").expect("a consumer add-on");
        // Which drags in ReShade, because the add-on loads through it.
        let reshade = position(&recipe, "reshade").expect("the injector");
        assert!(reshade < addon, "{recipe:#?}");
    }

    #[test]
    fn an_old_card_is_refused_before_anything_is_selected() {
        let catalog = default_catalog();
        let recipe = build(
            &catalog,
            Route::Feeder,
            &[Feature::NeuralRendering],
            Integration::None,
            &[],
            &nothing(),
            Some(Generation::Ampere),
        );

        assert!(recipe.steps.is_empty(), "{recipe:#?}");
        assert!(!recipe.refuses.is_empty());
    }

    #[test]
    fn every_step_says_why_it_is_there() {
        // A user is being asked to accept files into their game folder. Each
        // one has to be explainable, or the list is just trust-me.
        let catalog = default_catalog();
        for route in [Route::NativeSwap, Route::Bridge, Route::Feeder] {
            let recipe = build(
                &catalog,
                route,
                &Feature::ALL,
                Integration::None,
                &[Feature::RayReconstruction],
                &nothing(),
                Some(Generation::Blackwell),
            );
            for step in &recipe.steps {
                assert!(!step.because.is_empty(), "{route:?} {}", step.component);
                assert!(
                    catalog.get(&step.component).is_some(),
                    "{route:?} {} is not in the catalogue",
                    step.component
                );
            }
            // And no component is listed twice, which would mean installing it
            // over itself.
            let mut seen: Vec<&str> = recipe
                .steps
                .iter()
                .map(|step| step.component.as_str())
                .collect();
            let before = seen.len();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), before, "{route:?} repeats a component");
        }
    }

    #[test]
    fn the_order_is_stable_across_runs() {
        // The recipe is shown to a user before they agree to it, so it must
        // not shuffle between two identical questions.
        let catalog = default_catalog();
        let once = build(
            &catalog,
            Route::Feeder,
            &Feature::ALL,
            Integration::None,
            &[],
            &nothing(),
            Some(Generation::Blackwell),
        );
        let twice = build(
            &catalog,
            Route::Feeder,
            &Feature::ALL,
            Integration::None,
            &[],
            &nothing(),
            Some(Generation::Blackwell),
        );
        assert_eq!(once, twice);
    }
}
