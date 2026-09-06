//! Whether a game already has DLSS plumbing, read from its import table and
//! from the modules shipped beside it.
//!
//! This decides which install route can work, and it is evidence rather than
//! inference. NVIDIA's correctness check for a Streamline integration is a
//! dependency scan: an integrated application links `sl.interposer.dll`
//! *instead of* `dxgi.dll`, `d3d11.dll`, `d3d12.dll` or `vulkan-1.dll`.
//!
//! # Why the import table is not sufficient on its own
//!
//! That check is real, but it is **scoped**, and this module previously
//! applied it as though it were universal. The guide introduces it with "if
//! you are integrating Streamline by replacing the standard libraries with
//! `sl.interposer.lib`" - it is the validation step for *one* of the two
//! supported integration styles.
//!
//! The other style is manual hooking, and the guide is explicit about it:
//!
//! - "keep linking the standard libraries, load `sl.interposer.dll`
//!   dynamically and redirect DXGI/D3D API calls as required"
//! - "If you are using Vulkan, instead of `vulkan-1.dll` dynamically load
//!   `sl.interposer.dll`"
//!
//! A game integrated that way imports `d3d12.dll` or `vulkan-1.dll` and never
//! names anything `sl.*`, because the interposer arrives through
//! `LoadLibrary`. Judged on imports alone it looks like a game with no
//! Streamline at all - and for Vulkan, dynamic loading is the style NVIDIA
//! *recommends*, so this is not an exotic case.
//!
//! What settles it is that `sl.interposer.dll` and `sl.common.dll` are
//! mandatory redistributables which "need to be distributed with your
//! application", installed next to the host executable. Whatever the linkage,
//! the interposer is on disk. So a file beside the executable is decisive
//! evidence *for* Streamline, even though - as below - it is not good
//! evidence for anything else.
//!
//! # Why imports still lead
//!
//! For DLSS runtimes the import table remains strictly better than looking for
//! `nvngx_*.dll` beside the executable, which is what the tools in this space
//! do. Those files get left behind by older game versions and copied in by
//! hand, so presence does not imply use. `sl.interposer.dll` is different in
//! kind: nothing but a Streamline integration puts it there.
//!
//! See `docs/how-dlss-works.md` §6 for the sourcing and the reasoning.

use serde::{Deserialize, Serialize};

use crate::scan::api::Api;

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
    /// Take over the game's own upscaler through a proxy DLL, and run the
    /// neural pass over its output. No ReShade, and no synthesised inputs.
    ///
    /// The distinction from [`Route::Feeder`] is where the inputs come from:
    ///
    /// ```text
    /// feeder      game -> ReShade -> depth copy + motion-vector shader ->
    ///             synthetic contract -> DLSS
    /// optiscaler  game -> OptiScaler -> the game's own upscaler inputs -> DLSS
    /// ```
    ///
    /// So the feeder is always DLAA at native resolution, because it cannot
    /// make the game jitter its sampling; this route gets real upscaling,
    /// because the game is already doing it. It also runs the pass after the
    /// upscaler and *before* the interface is drawn, so the model never sees
    /// the HUD - which the feeder cannot avoid.
    ///
    /// Its cost is a hard requirement the feeder does not have: the game must
    /// already use DLSS, FSR 2/3 or XeSS. There has to be an upscaler call to
    /// take over. See [`Upscaler`].
    OptiScaler,
}

impl Route {
    pub const fn label(self) -> &'static str {
        match self {
            Route::NativeSwap => "replace the game's own runtime",
            Route::Bridge => "bridge the game's DLSS to a private DirectX 12 session",
            Route::Feeder => "build the inputs from ReShade",
            Route::OptiScaler => "take over the game's own upscaler",
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

/// An upscaler the game ships that is not DLSS.
///
/// This matters for exactly one thing: [`Route::OptiScaler`] can take over an
/// FSR or XeSS call and run DLSS in its place, so a game with no DLSS at all
/// still has a route that is not the feeder. Without one of these there is
/// nothing to take over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Upscaler {
    /// AMD FidelityFX Super Resolution 2 or 3.
    Fsr,
    /// Intel Xe Super Sampling.
    Xess,
}

impl Upscaler {
    pub const fn label(self) -> &'static str {
        match self {
            Upscaler::Fsr => "AMD FSR 2/3",
            Upscaler::Xess => "Intel XeSS",
        }
    }
}

/// The runtime DLLs AMD's SDK ships under.
const FSR_FILES: [&str; 8] = [
    "ffx_fsr2_api_x64.dll",
    "ffx_fsr2_api_dx12_x64.dll",
    "ffx_fsr2_api_vk_x64.dll",
    "ffx_fsr3upscaler_x64.dll",
    "ffx_fsr3_x64.dll",
    "amd_fidelityfx_dx12.dll",
    "amd_fidelityfx_vk.dll",
    "ffx_backend_dx12_x64.dll",
];

/// The runtime DLLs Intel's SDK ships under.
const XESS_FILES: [&str; 3] = ["libxess.dll", "libxess_dx11.dll", "libxess_fg.dll"];

/// Which non-DLSS upscaler a game ships, judged by the runtimes beside it.
///
/// A game shipping both reads as FSR: OptiScaler's FSR input hooks are the
/// ones it treats as primary.
///
/// **This can only ever be evidence for, never against.** A game that links
/// FSR statically ships none of these files and is indistinguishable from a
/// game with no upscaler at all, so `None` means "no evidence" rather than
/// "no upscaler".
///
/// The names deliberately come from the executable's own directory and no
/// deeper. OptiScaler's own package carries `libxess.dll` and
/// `amd_fidelityfx_*.dll` under an `OptiScaler/` subfolder - its bundled
/// upscalers, not the game's - and counting those would let a previous
/// install of our own manufacture the evidence for the next one.
pub fn ships_upscaler(beside: &[String]) -> Option<Upscaler> {
    let has = |list: &[&str]| {
        beside
            .iter()
            .any(|item| list.contains(&item.to_lowercase().as_str()))
    };
    if has(&FSR_FILES) {
        return Some(Upscaler::Fsr);
    }
    if has(&XESS_FILES) {
        return Some(Upscaler::Xess);
    }
    None
}

/// `imports` are lower-cased DLL names, as [`crate::pe`] reports them.
///
/// `beside` are lower-cased file names sitting in the executable's own
/// directory. Needed because manual hooking leaves no trace in the import
/// table; see the module documentation.
///
/// `has_native_dlss` is separate evidence - runtime files found beside the
/// executable - because a DX11 game can ship DLSS without linking Streamline,
/// and that is exactly the case the bridge route exists for.
/// `bitness` is 32 or 64, or `None` when the executable could not be read.
/// Only [`Route::OptiScaler`] depends on it, and an unknown bitness withholds
/// that route rather than guessing - offering a 64-bit-only component to a
/// 32-bit game produces a proxy DLL the loader silently refuses, which looks
/// exactly like the tool doing nothing.
pub fn assess(
    imports: &[String],
    beside: &[String],
    has_native_dlss: bool,
    bitness: Option<u8>,
    api: Option<Api>,
) -> Assessment {
    let imported = |name: &str| imports.iter().any(|item| item == name);
    let shipped = |name: &str| beside.iter().any(|item| item == name);

    // Worked out once, up front, because the branches below return early and
    // every one of them needs the same answer.
    let upscaler = ships_upscaler(beside);

    // Direct3D 10/11/12, which is what OptiScaler hooks.
    //
    // The import table alone is not enough, and the reason is the same one
    // this module opens with: a properly integrated Streamline game links
    // `sl.interposer.dll` *instead of* `dxgi.dll`, so it imports no Direct3D
    // at all. Judged on imports, Cyberpunk 2077 - a D3D12 game with a complete
    // Streamline integration, the exact case this route suits best - looked
    // like it had no Direct3D and was refused the route. The scan's own API
    // verdict falls back to strings in the binary for precisely that case, so
    // it leads here and the imports are the backstop.
    let direct3d = match api {
        Some(Api::Dxgi) => true,
        // Named rather than defaulted: Vulkan, D3D9, D3D8 and OpenGL are all
        // real answers that mean "not this route", and D3D10 has no injection
        // route at all.
        Some(Api::Vulkan | Api::D3d10 | Api::D3d9 | Api::D3d8 | Api::OpenGl) => false,
        None => ["d3d12.dll", "d3d11.dll", "dxgi.dll"]
            .iter()
            .any(|name| imported(name)),
    };
    // Every kind of evidence that this game has DLSS, not just the one the
    // caller measured. `has_native_dlss` is runtime files found on disk, which
    // is the weakest of the four and the only one an earlier version of this
    // gate consulted - so a game that *imports* NGX, or links Streamline, was
    // told it had nothing for OptiScaler to take over while plainly having it.
    let dlss_of_its_own = has_native_dlss
        || STREAMLINE
            .iter()
            .any(|name| imported(name) || shipped(name))
        || imports.iter().any(|name| name.starts_with("nvngx"));

    // 64-bit Direct3D 11 or 12 with something to take over. Vulkan and the
    // older Direct3D versions are excluded by requiring a D3D11/12 import
    // rather than by naming them, so a game that imports neither is withheld
    // rather than assumed.
    let optiscaler = bitness == Some(64) && direct3d && (dlss_of_its_own || upscaler.is_some());

    if STREAMLINE.iter().any(|name| imported(name)) {
        return Assessment {
            integration: Integration::Streamline,
            // The contract is already satisfied, so replacing files is the
            // whole job. The feeder would be manufacturing inputs the game is
            // already producing; OptiScaler would not - it reads those same
            // inputs - so it stays on the list as the second option.
            routes: with_optiscaler(vec![Route::NativeSwap], optiscaler),
            reason: "this game links NVIDIA Streamline, so it already provides everything \
                     the runtime needs"
                .to_owned(),
        };
    }

    // Manual hooking: the interposer is loaded at runtime, so it is on disk
    // but not in the import table. Streamline is integrated just as fully as
    // in the linked case, and the same route applies - the difference is only
    // in how the game got hold of it.
    if STREAMLINE.iter().any(|name| shipped(name)) {
        return Assessment {
            integration: Integration::Streamline,
            routes: with_optiscaler(vec![Route::NativeSwap], optiscaler),
            reason: "this game ships NVIDIA Streamline and loads it at startup rather than \
                     declaring it, so it already provides everything the runtime needs"
                .to_owned(),
        };
    }

    // NGX without Streamline: the feature runtimes are loaded directly.
    if imports.iter().any(|name| name.starts_with("nvngx")) {
        return Assessment {
            integration: Integration::NgxDirect,
            routes: with_optiscaler(vec![Route::NativeSwap], optiscaler),
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
            routes: with_optiscaler(vec![Route::Bridge], optiscaler)
                .into_iter()
                .chain([Route::Feeder])
                .collect(),
            reason: "this game has its own DLSS but not the newer plumbing, so its DLSS can \
                     be mirrored onto a private DirectX 12 session"
                .to_owned(),
        };
    }
    if has_native_dlss {
        return Assessment {
            integration: Integration::None,
            routes: with_optiscaler(vec![Route::NativeSwap], optiscaler)
                .into_iter()
                .chain([Route::Feeder])
                .collect(),
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

    // No DLSS, but an upscaler to take over. This is the case the OptiScaler
    // route exists for and the feeder handles badly: the game is already
    // producing depth, motion vectors and a jittered frame for its own
    // upscaler, so there is nothing to synthesise and nothing to run ReShade
    // for. It leads here rather than merely appearing on the list.
    if let Some(upscaler) = upscaler.filter(|_| optiscaler) {
        return Assessment {
            integration: Integration::None,
            routes: vec![Route::OptiScaler, Route::Feeder],
            reason: format!(
                "this game has no DLSS but ships {}, and those calls can be taken over and \
                 run as DLSS instead - so its own depth and motion vectors are used rather \
                 than rebuilt from ReShade",
                upscaler.label()
            ),
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

/// Appends [`Route::OptiScaler`] when the game can host it.
///
/// Always after the routes already there: those need no injected proxy and no
/// download, so where both work the simpler one is offered first. The one
/// place this order is reversed is a game with no DLSS at all, which is
/// handled directly rather than through here.
fn with_optiscaler(mut routes: Vec<Route>, eligible: bool) -> Vec<Route> {
    if eligible {
        routes.push(Route::OptiScaler);
    }
    routes
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
        let found = assess(
            &names(&["kernel32.dll", "sl.interposer.dll"]),
            &[],
            false,
            Some(64),
            None,
        );
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
            &[],
            false,
            Some(64),
            None,
        );
        assert_eq!(found.integration, Integration::Streamline);
    }

    #[test]
    fn manual_hooking_is_found_on_disk_when_the_imports_are_silent() {
        // NVIDIA's guide: for Vulkan, "instead of `vulkan-1.dll` dynamically
        // load `sl.interposer.dll`". Such a game imports the plain graphics
        // API and nothing else, so the import table alone reads as "no DLSS
        // plumbing" for a game with a complete Streamline integration.
        let imports = names(&["vulkan-1.dll", "kernel32.dll"]);
        let beside = names(&[
            "game.exe",
            "sl.interposer.dll",
            "sl.common.dll",
            "sl.dlss.dll",
        ]);

        let found = assess(&imports, &beside, true, Some(64), None);

        assert_eq!(found.integration, Integration::Streamline);
        assert_eq!(found.routes, vec![Route::NativeSwap]);

        // Without the on-disk evidence the same game is misread as needing a
        // bridge - an expensive route that manufactures inputs it already has.
        let blind = assess(&imports, &[], true, Some(64), None);
        assert_eq!(blind.integration, Integration::None);
        assert!(blind.routes.contains(&Route::Bridge));
    }

    #[test]
    fn a_runtime_beside_the_exe_is_not_mistaken_for_streamline() {
        // The on-disk check is deliberately narrow. DLSS runtimes get left
        // behind by old game versions and copied in by hand, so their presence
        // proves nothing about plumbing; only Streamline's own mandatory
        // modules are treated as decisive.
        let beside = names(&["game.exe", "nvngx_dlss.dll", "nvngx_dlssg.dll", "dxgi.dll"]);

        let found = assess(
            &names(&["d3d11.dll", "dxgi.dll"]),
            &beside,
            true,
            Some(64),
            None,
        );

        assert_ne!(found.integration, Integration::Streamline);
    }

    #[test]
    fn ngx_linked_directly_is_still_a_native_swap() {
        let found = assess(
            &names(&["d3d12.dll", "nvngx_dlss.dll"]),
            &[],
            false,
            Some(64),
            None,
        );
        assert_eq!(found.integration, Integration::NgxDirect);
        // Leads with the swap, and offers OptiScaler behind it: importing NGX
        // is proof this game has DLSS, so there is an upscaler call to take
        // over even though no runtime file was found beside the executable.
        assert_eq!(found.routes, vec![Route::NativeSwap, Route::OptiScaler]);
    }

    #[test]
    fn a_dx11_game_with_its_own_dlss_gets_the_bridge_first() {
        // The case the bridge exists for: real DLSS, but DX11, so neural
        // rendering cannot be hosted directly.
        let found = assess(
            &names(&["d3d11.dll", "dxgi.dll"]),
            &[],
            true,
            Some(64),
            None,
        );
        assert_eq!(found.routes.first(), Some(&Route::Bridge));
        assert!(found.routes.contains(&Route::Feeder));
    }

    #[test]
    fn a_vulkan_game_with_its_own_dlss_gets_the_bridge_first() {
        let found = assess(&names(&["vulkan-1.dll"]), &[], true, Some(64), None);
        assert_eq!(found.routes.first(), Some(&Route::Bridge));
    }

    #[test]
    fn a_dx12_game_with_runtime_files_tries_replacing_them_first() {
        let found = assess(
            &names(&["d3d12.dll", "dxgi.dll"]),
            &[],
            true,
            Some(64),
            None,
        );
        assert_eq!(found.routes.first(), Some(&Route::NativeSwap));
    }

    #[test]
    fn a_game_with_no_dlss_at_all_needs_the_feeder() {
        let found = assess(
            &names(&["d3d12.dll", "kernel32.dll"]),
            &[],
            false,
            Some(64),
            None,
        );
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
        let found = assess(
            &names(&["kernel32.dll", "user32.dll"]),
            &[],
            false,
            Some(64),
            None,
        );
        assert_eq!(found.integration, Integration::Undetermined);
        assert_eq!(found.routes, vec![Route::Feeder]);
        assert!(found.reason.contains("cannot tell"));

        // An empty table is the same situation.
        assert_eq!(
            assess(&[], &[], false, Some(64), None).integration,
            Integration::Undetermined
        );
    }

    #[test]
    fn a_declared_graphics_api_with_no_dlss_is_a_real_negative() {
        // The distinction that matters: here the executable did declare what
        // it loads, and DLSS is not among it.
        for api in ["d3d12.dll", "d3d11.dll", "vulkan-1.dll", "opengl32.dll"] {
            let found = assess(&names(&[api]), &[], false, Some(64), None);
            assert_eq!(found.integration, Integration::None, "{api}");
        }
    }

    #[test]
    fn a_game_with_no_dlss_but_an_upscaler_leads_with_optiscaler() {
        // The capability this route adds. Before it, such a game got the
        // feeder and nothing else - ReShade, synthetic motion vectors and
        // DLAA at native resolution - despite the game already producing
        // real depth and real vectors for its own upscaler every frame.
        let found = assess(
            &names(&["d3d12.dll", "dxgi.dll"]),
            &names(&["ffx_fsr3upscaler_x64.dll"]),
            false,
            Some(64),
            None,
        );

        assert_eq!(found.routes, vec![Route::OptiScaler, Route::Feeder]);
        assert!(
            found.reason.contains("AMD FSR 2/3"),
            "the user is told what was found: {}",
            found.reason
        );
    }

    #[test]
    fn xess_counts_as_much_as_fsr() {
        let found = assess(
            &names(&["d3d12.dll"]),
            &names(&["libxess.dll"]),
            false,
            Some(64),
            None,
        );
        assert_eq!(found.routes, vec![Route::OptiScaler, Route::Feeder]);
        assert!(found.reason.contains("Intel XeSS"), "{}", found.reason);
    }

    #[test]
    fn a_game_shipping_both_reads_as_fsr() {
        // OptiScaler treats its FSR input hooks as primary, so a game with
        // both is described the way the tool will actually drive it.
        assert_eq!(
            ships_upscaler(&names(&["libxess.dll", "ffx_fsr3_x64.dll"])),
            Some(Upscaler::Fsr)
        );
    }

    #[test]
    fn a_thirty_two_bit_game_is_never_offered_optiscaler() {
        // The component is 64-bit only. Offering it would produce a proxy DLL
        // the loader refuses without saying so, which looks exactly like the
        // tool having done nothing.
        let found = assess(
            &names(&["d3d12.dll"]),
            &names(&["ffx_fsr3_x64.dll"]),
            false,
            Some(32),
            None,
        );
        assert_eq!(found.routes, vec![Route::Feeder]);
    }

    #[test]
    fn an_unreadable_executable_withholds_the_route_rather_than_guessing() {
        let found = assess(
            &names(&["d3d12.dll"]),
            &names(&["ffx_fsr3_x64.dll"]),
            false,
            None,
            None,
        );
        assert_eq!(found.routes, vec![Route::Feeder]);
    }

    #[test]
    fn a_vulkan_game_is_never_offered_optiscaler() {
        // It hooks Direct3D. A Vulkan game with DLSS keeps the bridge, which
        // is the route that exists for exactly that case.
        let found = assess(&names(&["vulkan-1.dll"]), &[], true, Some(64), None);
        assert!(
            !found.routes.contains(&Route::OptiScaler),
            "{:?}",
            found.routes
        );
        assert_eq!(found.routes, vec![Route::Bridge, Route::Feeder]);
    }

    #[test]
    fn a_game_with_neither_dlss_nor_an_upscaler_has_nothing_to_take_over() {
        // The route's hard requirement. There has to be an upscaler call to
        // redirect; without one there is nothing for it to read.
        let found = assess(&names(&["d3d12.dll"]), &[], false, Some(64), None);
        assert_eq!(found.routes, vec![Route::Feeder]);
    }

    #[test]
    fn the_proven_route_is_still_offered_first_where_both_work() {
        // A D3D12 game with DLSS can take either. The native swap needs no
        // download and injects nothing, so it leads; OptiScaler follows as
        // the option rather than the default.
        let found = assess(
            &names(&["d3d12.dll", "nvngx_dlss.dll"]),
            &[],
            false,
            Some(64),
            None,
        );
        assert_eq!(found.routes, vec![Route::NativeSwap, Route::OptiScaler]);
    }

    #[test]
    fn a_streamline_game_that_imports_no_direct3d_still_gets_optiscaler() {
        // Found on a real game rather than reasoned about: Cyberpunk 2077
        // imports `sl.interposer.dll` and nothing else, because that is what a
        // correct Streamline integration looks like - the interposer replaces
        // the graphics import. An earlier version of this gate required a
        // Direct3D import and therefore refused the route to exactly the games
        // it suits best.
        let found = assess(
            &names(&["sl.interposer.dll"]),
            &[],
            false,
            Some(64),
            Some(Api::Dxgi),
        );
        assert_eq!(found.routes, vec![Route::NativeSwap, Route::OptiScaler]);
    }

    #[test]
    fn every_kind_of_dlss_evidence_reaches_the_optiscaler_route() {
        // A table rather than four separate cases, because the bug this
        // catches was a branch that simply never got the new route wired into
        // it: Ready or Not ships Streamline and loads it at startup, and its
        // branch was the one missed. Any future branch that returns a route
        // list has to satisfy this too.
        let cases: [(&str, Vec<String>, Vec<String>, bool); 4] = [
            (
                "streamline linked",
                names(&["sl.interposer.dll"]),
                vec![],
                false,
            ),
            (
                "streamline shipped and loaded at startup",
                names(&["dxgi.dll", "d3d12.dll"]),
                names(&["sl.interposer.dll", "sl.common.dll"]),
                false,
            ),
            (
                "ngx imported directly",
                names(&["d3d12.dll", "nvngx_dlss.dll"]),
                vec![],
                false,
            ),
            (
                "runtime files beside the executable",
                names(&["d3d12.dll"]),
                vec![],
                true,
            ),
        ];

        for (what, imports, beside, native) in cases {
            let found = assess(&imports, &beside, native, Some(64), Some(Api::Dxgi));
            assert!(
                found.routes.contains(&Route::OptiScaler),
                "{what}: {:?}",
                found.routes
            );
        }
    }

    #[test]
    fn the_api_verdict_outranks_a_leftover_import() {
        // A game whose imports name Direct3D but whose actual renderer is
        // Vulkan must not be offered a Direct3D hook. The verdict knows;
        // the import table does not.
        let found = assess(
            &names(&["d3d12.dll", "dxgi.dll"]),
            &names(&["ffx_fsr3_x64.dll"]),
            false,
            Some(64),
            Some(Api::Vulkan),
        );
        assert_eq!(found.routes, vec![Route::Feeder]);
    }

    #[test]
    fn an_older_direct3d_game_is_not_offered_the_route() {
        for api in [Api::D3d9, Api::D3d8, Api::D3d10, Api::OpenGl] {
            let found = assess(
                &names(&["d3d12.dll"]),
                &names(&["ffx_fsr3_x64.dll"]),
                false,
                Some(64),
                Some(api),
            );
            assert_eq!(found.routes, vec![Route::Feeder], "{api:?}");
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
        // And not this one, which is the whole point of it: it reads the
        // vectors the game already computes for its own upscaler.
        assert!(!Route::OptiScaler.needs_motion_vectors());
    }

    #[test]
    fn every_assessment_offers_at_least_one_route_and_a_reason() {
        for (imports, native) in [
            (vec![], false),
            (names(&["opengl32.dll"]), false),
            (names(&["d3d8.dll"]), true),
            (names(&["sl.common.dll"]), true),
        ] {
            let found = assess(&imports, &[], native, Some(64), None);
            assert!(!found.routes.is_empty(), "{imports:?}");
            assert!(!found.reason.is_empty(), "{imports:?}");
        }
    }
}
