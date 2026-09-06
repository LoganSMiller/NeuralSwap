//! Which graphics API an executable uses.
//!
//! This decides the install route, so getting it wrong means offering a user
//! an option that cannot work. Two sources of evidence, in order:
//!
//! 1. **The import table.** If the loader will resolve `d3d12.dll`, the game
//!    uses Direct3D 12. This is authoritative and cheap.
//! 2. **Strings in the mapped sections.** A game that reaches Direct3D through
//!    `LoadLibrary` has no import entry at all, but the entry-point name it
//!    asks for is still in the binary.
//!
//! Imports are preferred because a string can be a leftover: engines ship code
//! paths for renderers they no longer use, and Rockstar's launcher carries a
//! D3D9 marker in a game that runs on D3D11. Falling back to markers only when
//! the import table is silent is what keeps those from winning.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Api {
    /// Direct3D 10/11/12, all of which are reached through a DXGI swapchain,
    /// and all of which take the same `dxgi.dll` proxy.
    Dxgi,
    /// Direct3D 10 specifically, which no injection route supports.
    D3d10,
    D3d9,
    D3d8,
    Vulkan,
    OpenGl,
}

impl Api {
    /// The proxy DLL name an injector installs for this API.
    pub const fn hook_name(self) -> &'static str {
        match self {
            Api::OpenGl => "opengl32.dll",
            Api::D3d9 => "d3d9.dll",
            Api::D3d8 => "d3d8.dll",
            // Vulkan is registered as a layer rather than proxied, but the
            // DXGI name is what the DX bridge installs.
            Api::Dxgi | Api::D3d10 | Api::Vulkan => "dxgi.dll",
        }
    }
}

impl fmt::Display for Api {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Api::Dxgi => "dxgi",
            Api::D3d10 => "d3d10",
            Api::D3d9 => "d3d9",
            Api::D3d8 => "d3d8",
            Api::Vulkan => "vulkan",
            Api::OpenGl => "opengl",
        })
    }
}

/// Which Direct3D a DXGI game actually uses.
///
/// [`Api`] deliberately folds 10, 11 and 12 together, because the question it
/// answers is "which proxy DLL does an injector install" and the answer for
/// all three is `dxgi.dll`. This is the other question, and several things
/// turn on it:
///
/// - `sl.dlss_g` and `sl.dlss_nr` list `d3d12, vk` in their manifests and
///   refuse Direct3D 11 outright, so a feature can be fully fed by a D3D11
///   game and still be unreachable.
/// - AMD's FSR 3.1 frame generation is Direct3D 12 only.
///
/// This used to exist only as display text - the detection below knew perfectly
/// well whether it had seen `d3d12.dll` and put the answer in a label for a
/// human to read, where no logic could reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Direct3D {
    Eleven,
    Twelve,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub api: Api,
    /// What to show a person: "DirectX 12", not "dxgi".
    pub label: String,
    /// True when the verdict came from a string rather than the import table,
    /// which is weaker evidence and worth surfacing in diagnostics.
    pub from_marker: bool,
    /// The Direct3D version, when it is known.
    ///
    /// `None` for a game that reaches DXGI without naming a device DLL: the
    /// swapchain is evidence of Direct3D and says nothing about which one, and
    /// guessing would be worse than admitting it.
    #[serde(default)]
    pub direct3d: Option<Direct3D>,
}

fn verdict(api: Api, label: &str, from_marker: bool) -> Verdict {
    Verdict {
        api,
        label: label.to_owned(),
        from_marker,
        direct3d: None,
    }
}

/// A [`Verdict`] that also knows which Direct3D it saw.
fn d3d_verdict(label: &str, level: Direct3D, from_marker: bool) -> Verdict {
    Verdict {
        direct3d: Some(level),
        ..verdict(Api::Dxgi, label, from_marker)
    }
}

/// Entry-point and SDK strings worth searching for when the import table is
/// silent. Kept together with the detection that consumes them so the two
/// cannot drift.
pub const MARKERS: &[&str] = &[
    // Agility SDK titles can export only the SDK path or version from the
    // launcher executable and resolve D3D12 in the engine later, so those
    // exports are authoritative D3D12 evidence too.
    "D3D12CreateDevice",
    "D3D12SDKPath",
    "D3D12SDKVersion",
    "D3D11CreateDevice",
    "D3D10CreateDevice",
    "Direct3DCreate9",
    "Direct3DCreate8",
    "CreateDXGIFactory",
    "vkCreateInstance",
    "wglCreateContext",
];

/// Decide the API from a binary's imports and markers.
///
/// `imports` are lower-cased DLL names; `markers` are the subset of [`MARKERS`]
/// found in the image.
pub fn detect(imports: &[String], markers: &[String]) -> Option<Verdict> {
    let imported = |name: &str| imports.iter().any(|item| item == name);

    // Highest version first: a game importing both d3d11 and d3d12 is a D3D12
    // game with a fallback path, and the higher one is what it will use.
    if imported("d3d12.dll") {
        return Some(d3d_verdict("DirectX 12", Direct3D::Twelve, false));
    }
    if imported("d3d11.dll") {
        return Some(d3d_verdict("DirectX 11", Direct3D::Eleven, false));
    }
    if imported("d3d10.dll") || imported("d3d10_1.dll") {
        return Some(verdict(Api::D3d10, "DirectX 10", false));
    }
    if imported("dxgi.dll") {
        // DXGI without a versioned device DLL: the swapchain is there but the
        // device is created dynamically. Proxying DXGI still works.
        return Some(verdict(Api::Dxgi, "DirectX (DXGI)", false));
    }
    if imported("vulkan-1.dll") {
        return Some(verdict(Api::Vulkan, "Vulkan", false));
    }
    if imported("d3d9.dll") {
        return Some(verdict(Api::D3d9, "DirectX 9", false));
    }
    if imported("d3d8.dll") {
        return Some(verdict(Api::D3d8, "DirectX 8", false));
    }
    if imported("opengl32.dll") {
        return Some(verdict(Api::OpenGl, "OpenGL", false));
    }

    detect_from_markers(markers)
}

fn detect_from_markers(markers: &[String]) -> Option<Verdict> {
    let has = |name: &str| markers.iter().any(|item| item == name);

    if has("D3D12CreateDevice") || has("D3D12SDKPath") || has("D3D12SDKVersion") {
        return Some(d3d_verdict("DirectX 12", Direct3D::Twelve, true));
    }
    if has("D3D11CreateDevice") {
        return Some(d3d_verdict("DirectX 11", Direct3D::Eleven, true));
    }
    if has("D3D10CreateDevice") {
        return Some(verdict(Api::D3d10, "DirectX 10", true));
    }
    if has("CreateDXGIFactory") {
        return Some(verdict(Api::Dxgi, "DirectX (DXGI)", true));
    }
    if has("Direct3DCreate9") {
        return Some(verdict(Api::D3d9, "DirectX 9", true));
    }
    if has("Direct3DCreate8") {
        return Some(verdict(Api::D3d8, "DirectX 8", true));
    }
    if has("vkCreateInstance") {
        return Some(verdict(Api::Vulkan, "Vulkan", true));
    }
    if has("wglCreateContext") {
        return Some(verdict(Api::OpenGl, "OpenGL", true));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_direct3d_version_is_recorded_rather_than_only_displayed() {
        // It used to exist only inside the label, where a person could read it
        // and no rule could. Several features refuse Direct3D 11 outright, so
        // this has to be a value.
        let twelve = detect(&names(&["d3d12.dll"]), &[]).expect("verdict");
        assert_eq!(twelve.direct3d, Some(Direct3D::Twelve));

        let eleven = detect(&names(&["d3d11.dll"]), &[]).expect("verdict");
        assert_eq!(eleven.direct3d, Some(Direct3D::Eleven));

        // Both are still the same proxy, which is what `api` answers.
        assert_eq!(twelve.api, Api::Dxgi);
        assert_eq!(eleven.api, Api::Dxgi);
    }

    #[test]
    fn a_game_with_only_a_swapchain_admits_it_does_not_know() {
        // DXGI without a versioned device DLL. The swapchain proves Direct3D
        // and says nothing about which one, and guessing D3D12 here would
        // offer features a D3D11 game cannot run.
        let found = detect(&names(&["dxgi.dll"]), &[]).expect("verdict");
        assert_eq!(found.api, Api::Dxgi);
        assert_eq!(found.direct3d, None);
    }

    #[test]
    fn a_marker_carries_the_version_as_well_as_the_import_does() {
        let found = detect(&[], &names(&["D3D12CreateDevice"])).expect("verdict");
        assert_eq!(found.direct3d, Some(Direct3D::Twelve));
        assert!(found.from_marker);
    }

    #[test]
    fn the_import_table_decides_when_it_can() {
        let found = detect(&names(&["kernel32.dll", "d3d12.dll"]), &[]).expect("verdict");
        assert_eq!(found.api, Api::Dxgi);
        assert_eq!(found.label, "DirectX 12");
        assert!(!found.from_marker);
    }

    #[test]
    fn the_highest_imported_version_wins() {
        // A D3D12 game with a D3D11 fallback path is a D3D12 game.
        let found = detect(&names(&["d3d11.dll", "d3d12.dll"]), &[]).expect("verdict");
        assert_eq!(found.label, "DirectX 12");

        let found = detect(&names(&["d3d10.dll", "d3d11.dll"]), &[]).expect("verdict");
        assert_eq!(found.label, "DirectX 11");
    }

    #[test]
    fn imports_beat_a_leftover_marker() {
        // Rockstar's launcher carries a D3D9 marker in a game that runs D3D11.
        // Preferring the import table is what stops the string winning.
        let found = detect(
            &names(&["d3d11.dll"]),
            &names(&["Direct3DCreate9", "D3D11CreateDevice"]),
        )
        .expect("verdict");
        assert_eq!(found.label, "DirectX 11");
        assert!(!found.from_marker);
    }

    #[test]
    fn markers_are_the_fallback_for_a_dynamically_loaded_renderer() {
        let found =
            detect(&names(&["kernel32.dll"]), &names(&["D3D12CreateDevice"])).expect("verdict");
        assert_eq!(found.api, Api::Dxgi);
        assert_eq!(found.label, "DirectX 12");
        // Weaker evidence, and the verdict says so.
        assert!(found.from_marker);
    }

    #[test]
    fn an_agility_sdk_launcher_is_recognised_as_directx_12() {
        // The launcher exports only the SDK path and resolves D3D12 later.
        for marker in ["D3D12SDKPath", "D3D12SDKVersion"] {
            let found = detect(&names(&["kernel32.dll"]), &names(&[marker]))
                .unwrap_or_else(|| panic!("no verdict for {marker}"));
            assert_eq!(found.label, "DirectX 12", "{marker}");
        }
    }

    #[test]
    fn directx_10_is_identified_rather_than_folded_into_dxgi() {
        // No injection route supports DX10, so it must not be reported as a
        // generic DXGI target that then fails at install time.
        let found = detect(&names(&["d3d10.dll"]), &[]).expect("verdict");
        assert_eq!(found.api, Api::D3d10);
        assert_eq!(found.label, "DirectX 10");
    }

    #[test]
    fn dxgi_alone_is_reported_as_unversioned() {
        let found = detect(&names(&["dxgi.dll"]), &[]).expect("verdict");
        assert_eq!(found.api, Api::Dxgi);
        assert_eq!(found.label, "DirectX (DXGI)");
    }

    #[test]
    fn legacy_and_cross_platform_apis_are_recognised() {
        assert_eq!(
            detect(&names(&["d3d9.dll"]), &[]).map(|v| v.api),
            Some(Api::D3d9)
        );
        assert_eq!(
            detect(&names(&["d3d8.dll"]), &[]).map(|v| v.api),
            Some(Api::D3d8)
        );
        assert_eq!(
            detect(&names(&["vulkan-1.dll"]), &[]).map(|v| v.api),
            Some(Api::Vulkan)
        );
        assert_eq!(
            detect(&names(&["opengl32.dll"]), &[]).map(|v| v.api),
            Some(Api::OpenGl)
        );
    }

    #[test]
    fn a_binary_with_no_graphics_evidence_gets_no_verdict() {
        assert!(detect(&names(&["kernel32.dll", "user32.dll"]), &[]).is_none());
        assert!(detect(&[], &[]).is_none());
    }

    #[test]
    fn hook_names_match_the_api() {
        assert_eq!(Api::Dxgi.hook_name(), "dxgi.dll");
        assert_eq!(Api::D3d9.hook_name(), "d3d9.dll");
        assert_eq!(Api::D3d8.hook_name(), "d3d8.dll");
        assert_eq!(Api::OpenGl.hook_name(), "opengl32.dll");
    }

    #[test]
    fn every_marker_the_detection_reads_is_in_the_published_list() {
        // The scanner searches for MARKERS and the detection consults specific
        // names; a name in one and not the other is silently dead logic.
        for name in [
            "D3D12CreateDevice",
            "D3D12SDKPath",
            "D3D12SDKVersion",
            "D3D11CreateDevice",
            "D3D10CreateDevice",
            "CreateDXGIFactory",
            "Direct3DCreate9",
            "Direct3DCreate8",
            "vkCreateInstance",
            "wglCreateContext",
        ] {
            assert!(
                MARKERS.contains(&name),
                "{name} is read but never searched for"
            );
        }
    }
}
