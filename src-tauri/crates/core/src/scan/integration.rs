//! Whether a game already has DLSS plumbing, read from its import table.
//!
//! This decides which install route can work, and it is evidence rather than
//! inference. NVIDIA's own correctness check for a Streamline integration is a
//! dependency scan: an integrated application links `sl.interposer.dll`
//! *instead of* `dxgi.dll`, `d3d11.dll`, `d3d12.dll` or `vulkan-1.dll`, and
//! the programming guide says in as many words that seeing one of those, or
//! not seeing the interposer, means Streamline was not integrated correctly.
//!
//! Reading the import table is strictly better than looking for `nvngx_*.dll`
//! files beside the executable, which is what the tools in this space do. A
//! file can be present and unused - left behind by an older game version, or
//! copied there by hand - whereas the import table describes what the loader
//! will actually resolve.
//!
//! See `docs/how-dlss-works.md` §6 for the sourcing and the reasoning.

use serde::{Deserialize, Serialize};

/// How a game reaches DLSS, if it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Integration {
    /// Links `sl.interposer.dll`: Streamline is integrated, so the game
    /// already produces every tagged resource a feature needs.
    Streamline,
    /// Calls NGX directly, without Streamline brokering it.
    NgxDirect,
    /// Graphics API only. Whatever DLSS this game has, it is not reachable
    /// through plumbing we can see from here.
    None,
    /// The import table names no graphics API at all, so it says nothing.
    ///
    /// Normal and common: Unity and many engines resolve Direct3D through
    /// `LoadLibrary` at startup, so nothing appears statically. An absent
    /// import is not evidence of an absent feature, and this is reported as
    /// what it is rather than folded into `None`.
    Undetermined,
}

/// What can be installed, given the integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Route {
    /// Replace the runtime files. The game already satisfies the input
    /// contract, so this is a file operation and nothing has to be synthesised.
    NativeSwap,
    /// Mirror the game's own DLSS onto a private D3D12 session, for a DX11 or
    /// Vulkan game that has DLSS but cannot host neural rendering directly.
    Bridge,
    /// Manufacture the whole contract from ReShade's depth buffer and computed
    /// motion vectors, for a game with no DLSS at all.
    Feeder,
}

impl Route {
    pub const fn label(self) -> &'static str {
        match self {
            Route::NativeSwap => "replace the game's own runtime",
            Route::Bridge => "bridge the game's DLSS to a private DirectX 12 session",
            Route::Feeder => "build the inputs from ReShade",
        }
    }

    /// Whether this route needs motion vectors computed by a shader pack.
    ///
    /// Only the Feeder route does, and for it the dependency is hard: DLSS
    /// requires `kBufferTypeMotionVectors`, a game that never had DLSS has no
    /// reason to expose any, so they must be derived by optical flow. An
    /// install without a motion-estimation pack does not degrade - it does
    /// not work.
    pub const fn needs_motion_vectors(self) -> bool {
        matches!(self, Route::Feeder)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Assessment {
    pub integration: Integration,
    /// Routes that could work, best first.
    pub routes: Vec<Route>,
    /// Why, in terms a user can act on.
    pub reason: String,
}

/// Names that mean Streamline is linked. The interposer is the one NVIDIA
/// documents; the others appear when a game links individual plugins.
const STREAMLINE: [&str; 2] = ["sl.interposer.dll", "sl.common.dll"];

/// `imports` are lower-cased DLL names, as [`crate::pe`] reports them.
///
/// `has_native_dlss` is separate evidence - runtime files found beside the
/// executable - because a DX11 game can ship DLSS without linking Streamline,
/// and that is exactly the case the bridge route exists for.
pub fn assess(imports: &[String], has_native_dlss: bool) -> Assessment {
    let imported = |name: &str| imports.iter().any(|item| item == name);

    if STREAMLINE.iter().any(|name| imported(name)) {
        return Assessment {
            integration: Integration::Streamline,
            // The contract is already satisfied, so replacing files is the
            // whole job. The other routes would be manufacturing inputs the
            // game is already producing.
            routes: vec![Route::NativeSwap],
            reason: "this game links NVIDIA Streamline, so it already provides everything \
                     the runtime needs"
                .to_owned(),
        };
    }

    // NGX without Streamline: the feature runtimes are loaded directly.
    if imports.iter().any(|name| name.starts_with("nvngx")) {
        return Assessment {
            integration: Integration::NgxDirect,
            routes: vec![Route::NativeSwap],
            reason: "this game loads the DLSS runtime directly".to_owned(),
        };
    }

    let vulkan = imported("vulkan-1.dll");
    let dx11 = imported("d3d11.dll") && !imported("d3d12.dll");

    // Has DLSS but no Streamline plumbing we can see. Mirroring its real DLSS
    // yields a genuine contract, which beats synthesising one.
    if has_native_dlss && (dx11 || vulkan) {
        return Assessment {
            integration: Integration::None,
            routes: vec![Route::Bridge, Route::Feeder],
            reason: "this game has its own DLSS but not the newer plumbing, so its DLSS can \
                     be mirrored onto a private DirectX 12 session"
                .to_owned(),
        };
    }
    if has_native_dlss {
        return Assessment {
            integration: Integration::None,
            routes: vec![Route::NativeSwap, Route::Feeder],
            reason: "DLSS runtime files are present beside the executable, so replacing them \
                     is worth trying first"
                .to_owned(),
        };
    }

    // Nothing statically imported that names a graphics API. The executable
    // resolves one at runtime, so the import table is silent rather than
    // negative - and saying "this game has no DLSS" on that basis would be
    // claiming evidence we do not have.
    const GRAPHICS: [&str; 9] = [
        "d3d12.dll",
        "d3d11.dll",
        "d3d10.dll",
        "d3d10_1.dll",
        "dxgi.dll",
        "vulkan-1.dll",
        "d3d9.dll",
        "d3d8.dll",
        "opengl32.dll",
    ];
    if !GRAPHICS.iter().any(|name| imported(name)) {
        return Assessment {
            integration: Integration::Undetermined,
            routes: vec![Route::Feeder],
            reason: "this game loads its graphics API at startup rather than declaring it, so \
                     we cannot tell from the file whether it has DLSS - building the inputs \
                     from ReShade is the route that works either way"
                .to_owned(),
        };
    }

    Assessment {
        integration: Integration::None,
        routes: vec![Route::Feeder],
        reason: "this game has no DLSS of its own, so the inputs have to be built from \
                 ReShade's depth buffer and computed motion vectors"
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn the_interposer_is_decisive() {
        // NVIDIA's own integration check: the interposer replaces the graphics
        // library, so its presence settles the question.
        let found = assess(&names(&["kernel32.dll", "sl.interposer.dll"]), false);
        assert_eq!(found.integration, Integration::Streamline);
        assert_eq!(found.routes, vec![Route::NativeSwap]);
    }

    #[test]
    fn streamline_beats_a_leftover_graphics_import() {
        // A correctly integrated game should not import dxgi at all, but a
        // partial integration can leave one behind. The interposer still wins:
        // the plumbing is there either way.
        let found = assess(
            &names(&["dxgi.dll", "d3d12.dll", "sl.interposer.dll"]),
            false,
        );
        assert_eq!(found.integration, Integration::Streamline);
    }

    #[test]
    fn ngx_linked_directly_is_still_a_native_swap() {
        let found = assess(&names(&["d3d12.dll", "nvngx_dlss.dll"]), false);
        assert_eq!(found.integration, Integration::NgxDirect);
        assert_eq!(found.routes, vec![Route::NativeSwap]);
    }

    #[test]
    fn a_dx11_game_with_its_own_dlss_gets_the_bridge_first() {
        // The case the bridge exists for: real DLSS, but DX11, so neural
        // rendering cannot be hosted directly.
        let found = assess(&names(&["d3d11.dll", "dxgi.dll"]), true);
        assert_eq!(found.routes.first(), Some(&Route::Bridge));
        assert!(found.routes.contains(&Route::Feeder));
    }

    #[test]
    fn a_vulkan_game_with_its_own_dlss_gets_the_bridge_first() {
        let found = assess(&names(&["vulkan-1.dll"]), true);
        assert_eq!(found.routes.first(), Some(&Route::Bridge));
    }

    #[test]
    fn a_dx12_game_with_runtime_files_tries_replacing_them_first() {
        let found = assess(&names(&["d3d12.dll", "dxgi.dll"]), true);
        assert_eq!(found.routes.first(), Some(&Route::NativeSwap));
    }

    #[test]
    fn a_game_with_no_dlss_at_all_needs_the_feeder() {
        let found = assess(&names(&["d3d12.dll", "kernel32.dll"]), false);
        assert_eq!(found.integration, Integration::None);
        assert_eq!(found.routes, vec![Route::Feeder]);
        assert!(found.reason.contains("motion vectors"));
    }

    #[test]
    fn a_silent_import_table_is_undetermined_rather_than_negative() {
        // Every Unity title on the development machine looks like this: the
        // graphics API is resolved with LoadLibrary at startup, so nothing is
        // imported statically. Claiming "this game has no DLSS" on that basis
        // would be asserting evidence we do not have.
        let found = assess(&names(&["kernel32.dll", "user32.dll"]), false);
        assert_eq!(found.integration, Integration::Undetermined);
        assert_eq!(found.routes, vec![Route::Feeder]);
        assert!(found.reason.contains("cannot tell"));

        // An empty table is the same situation.
        assert_eq!(assess(&[], false).integration, Integration::Undetermined);
    }

    #[test]
    fn a_declared_graphics_api_with_no_dlss_is_a_real_negative() {
        // The distinction that matters: here the executable did declare what
        // it loads, and DLSS is not among it.
        for api in ["d3d12.dll", "d3d11.dll", "vulkan-1.dll", "opengl32.dll"] {
            let found = assess(&names(&[api]), false);
            assert_eq!(found.integration, Integration::None, "{api}");
        }
    }

    #[test]
    fn only_the_feeder_route_needs_motion_vectors() {
        // DLSS requires kBufferTypeMotionVectors. A game that never had DLSS
        // exposes none, so they are computed - a hard dependency, not a
        // nicety, and the only route that carries it.
        assert!(Route::Feeder.needs_motion_vectors());
        assert!(!Route::Bridge.needs_motion_vectors());
        assert!(!Route::NativeSwap.needs_motion_vectors());
    }

    #[test]
    fn every_assessment_offers_at_least_one_route_and_a_reason() {
        for (imports, native) in [
            (vec![], false),
            (names(&["opengl32.dll"]), false),
            (names(&["d3d8.dll"]), true),
            (names(&["sl.common.dll"]), true),
        ] {
            let found = assess(&imports, native);
            assert!(!found.routes.is_empty(), "{imports:?}");
            assert!(!found.reason.is_empty(), "{imports:?}");
        }
    }
}
