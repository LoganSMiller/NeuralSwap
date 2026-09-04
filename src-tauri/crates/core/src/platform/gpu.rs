//! What graphics hardware this machine has.
//!
//! Read for one reason: to stop somebody installing a runtime their card
//! cannot run. A DLSS feature gated to a GPU generation is gated because it
//! needs hardware the earlier cards do not have, so an install that ignores
//! that produces a game which crashes on launch or silently falls back - and
//! the user blames whatever touched the folder last, which would be us.
//!
//! Read from the display-adapter registry class rather than by enumerating
//! DXGI. The registry is already reachable through the dependency we have, the
//! answer does not need a device or a swap chain, and this runs during a
//! preflight where creating a D3D device would be a strange thing to do. The
//! cost is that the adapter is identified by its driver description string, so
//! generation detection is inference from a name - which is why an
//! unrecognised name reports `Unknown` and never blocks anything.

use serde::{Deserialize, Serialize};

/// The NVIDIA architecture generations that matter for feature gating.
///
/// Only NVIDIA is enumerated because only NVIDIA ships DLSS. An AMD or Intel
/// card is `NotNvidia`, which is a definite answer rather than a gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Generation {
    /// Pre-RTX. No tensor cores, so no DLSS of any kind.
    PreTuring,
    /// GTX 16-series: Turing silicon without the RT cores.
    TuringNoRt,
    /// RTX 20-series.
    Turing,
    /// RTX 30-series.
    Ampere,
    /// RTX 40-series.
    Ada,
    /// RTX 50-series.
    Blackwell,
    /// Newer than anything this build knows about. Deliberately ordered above
    /// `Blackwell` so a future card is never treated as too old.
    NewerThanKnown,
    NotNvidia,
    /// The adapter could not be read, or its name was not recognised.
    Unknown,
}

impl Generation {
    /// A name for the UI. Not a marketing name - the series number is what a
    /// user recognises about their own card.
    pub const fn label(self) -> &'static str {
        match self {
            Generation::PreTuring => "older than GeForce 16-series",
            Generation::TuringNoRt => "GeForce GTX 16-series",
            Generation::Turing => "GeForce RTX 20-series",
            Generation::Ampere => "GeForce RTX 30-series",
            Generation::Ada => "GeForce RTX 40-series",
            Generation::Blackwell => "GeForce RTX 50-series",
            Generation::NewerThanKnown => "a newer NVIDIA card",
            Generation::NotNvidia => "not an NVIDIA card",
            Generation::Unknown => "unrecognised",
        }
    }

    /// Whether this generation is known to be at least `floor`.
    ///
    /// `Unknown` answers `true`: an unreadable adapter is not evidence of old
    /// hardware, and refusing an install because we could not identify a card
    /// would break perfectly good machines. The preflight reports the
    /// uncertainty instead.
    pub fn at_least(self, floor: Generation) -> bool {
        match self {
            Generation::Unknown => true,
            Generation::NotNvidia => false,
            known => known >= floor,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Adapter {
    /// The driver's own description, e.g. "NVIDIA GeForce RTX 4070 Ti".
    pub name: String,
    pub generation: Generation,
    /// The Windows driver version, e.g. "32.0.15.6109".
    pub driver: Option<String>,
    /// The NVIDIA-facing version, e.g. "561.09", derived from the Windows one.
    /// NVIDIA's release notes and every support forum speak this dialect, so
    /// showing only the Windows quad would make a user's own driver
    /// unrecognisable to them.
    pub nvidia_driver: Option<String>,
}

/// Every display adapter the machine reports.
///
/// A laptop has two - the integrated one and the discrete one - and which is
/// "the" GPU depends on power settings we cannot see from here. So all of them
/// are returned and the caller decides; [`best_nvidia`] picks the one that
/// matters for a DLSS decision.
pub fn adapters() -> Vec<Adapter> {
    #[cfg(windows)]
    {
        // The display-adapter setup class. Each numbered subkey is one
        // adapter's driver registration.
        const CLASS: &str =
            r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";

        let Ok(class) = windows_registry::LOCAL_MACHINE.open(CLASS) else {
            return Vec::new();
        };

        let mut found = Vec::new();
        // Numbered rather than enumerated: the class also holds named keys
        // (Configuration, Properties) that are not adapters, and a fixed
        // range avoids depending on the crate's enumeration surface.
        for index in 0..16_u32 {
            let Ok(entry) = class.open(format!("{index:04}")) else {
                continue;
            };
            let Some(name) = entry
                .get_string("DriverDesc")
                .ok()
                .or_else(|| entry.get_string("HardwareInformation.AdapterString").ok())
            else {
                continue;
            };
            if name.trim().is_empty() {
                continue;
            }
            let driver = entry.get_string("DriverVersion").ok();
            let generation = classify(&name);
            found.push(Adapter {
                // Only meaningful for an NVIDIA adapter. Translating any
                // vendor's quad produces a plausible-looking number - an Intel
                // iGPU on this machine reported "NVIDIA 188.61" - which is
                // worse than showing nothing, because it looks like a fact.
                nvidia_driver: match generation {
                    Generation::NotNvidia | Generation::Unknown => None,
                    _ => driver.as_deref().and_then(nvidia_driver_version),
                },
                generation,
                driver,
                name,
            });
        }
        found
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// The adapter a DLSS decision should be made against: the newest NVIDIA card
/// present. `None` when there is no NVIDIA adapter, or none could be read.
pub fn best_nvidia() -> Option<Adapter> {
    adapters()
        .into_iter()
        .filter(|adapter| {
            !matches!(
                adapter.generation,
                Generation::NotNvidia | Generation::Unknown
            )
        })
        .max_by_key(|adapter| adapter.generation)
}

/// Infer the generation from a driver description string.
///
/// Matched on the series number rather than a table of model names, because a
/// table would be out of date the week a new card ships. The series number has
/// been the stable part of NVIDIA's naming for a decade.
pub fn classify(name: &str) -> Generation {
    let lower = name.to_lowercase();
    if !(lower.contains("nvidia")
        || lower.contains("geforce")
        || lower.contains("quadro")
        || lower.contains("tesla"))
    {
        return Generation::NotNvidia;
    }

    // The first four-digit run that looks like a model number. `RTX 4070 Ti`
    // and `RTX A4000` both appear, so the digits are found rather than assumed
    // to sit at a fixed offset.
    if let Some(model) = model_number(&lower) {
        return match model / 1000 {
            2 => Generation::Turing,
            3 => Generation::Ampere,
            4 => Generation::Ada,
            5 => Generation::Blackwell,
            // 6000-series and beyond: newer than this build knows, and must
            // not be mistaken for something old.
            6..=9 => Generation::NewerThanKnown,
            1 => {
                // 16-series is Turing without RT cores; 10-series is Pascal.
                if (1600..1700).contains(&model) {
                    Generation::TuringNoRt
                } else {
                    Generation::PreTuring
                }
            }
            _ => Generation::Unknown,
        };
    }
    Generation::Unknown
}

/// The first run of exactly four digits, as a number.
fn model_number(lower: &str) -> Option<u32> {
    let bytes = lower.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index - start == 4 {
            return lower.get(start..index)?.parse().ok();
        }
    }
    None
}

/// Turn a Windows driver quad into the version NVIDIA publishes.
///
/// Windows reports something like `32.0.15.6109`; NVIDIA calls that `561.09`.
/// The mapping is the last five digits of the final two components, split
/// three-and-two - a convention, not a documented formula, so anything that
/// does not fit returns `None` rather than a guess.
pub fn nvidia_driver_version(windows_version: &str) -> Option<String> {
    let mut parts = windows_version.split('.');
    let third = parts.nth(2)?;
    let fourth = parts.next()?;
    if !third.bytes().all(|b| b.is_ascii_digit()) || !fourth.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Concatenate then take the last five: the third component contributes its
    // trailing digit for versions past 999.
    let joined = format!("{third}{fourth}");
    let tail = joined.get(joined.len().checked_sub(5)?..)?;
    let (major, minor) = tail.split_at(3);
    Some(format!("{}.{minor}", major.trim_start_matches('0')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_numbers_map_to_generations() {
        assert_eq!(classify("NVIDIA GeForce RTX 5090"), Generation::Blackwell);
        assert_eq!(classify("NVIDIA GeForce RTX 4070 Ti"), Generation::Ada);
        assert_eq!(
            classify("NVIDIA GeForce RTX 3080 Laptop GPU"),
            Generation::Ampere
        );
        assert_eq!(
            classify("NVIDIA GeForce RTX 2060 SUPER"),
            Generation::Turing
        );
        assert_eq!(
            classify("NVIDIA GeForce GTX 1660 Ti"),
            Generation::TuringNoRt
        );
        assert_eq!(
            classify("NVIDIA GeForce GTX 1080 Ti"),
            Generation::PreTuring
        );
    }

    #[test]
    fn a_card_newer_than_this_build_is_not_treated_as_old() {
        // The failure that would matter in two years: a 60-series card falling
        // into a bucket that blocks an install it can obviously run.
        assert_eq!(
            classify("NVIDIA GeForce RTX 6080"),
            Generation::NewerThanKnown
        );
        assert!(Generation::NewerThanKnown.at_least(Generation::Blackwell));
    }

    #[test]
    fn other_vendors_are_a_definite_answer_not_a_gap() {
        assert_eq!(classify("AMD Radeon RX 7900 XTX"), Generation::NotNvidia);
        assert_eq!(
            classify("Intel(R) Arc(TM) A770 Graphics"),
            Generation::NotNvidia
        );
        assert_eq!(classify("Intel(R) UHD Graphics 630"), Generation::NotNvidia);
        assert!(!Generation::NotNvidia.at_least(Generation::Turing));
    }

    #[test]
    fn an_unrecognised_nvidia_name_never_blocks_an_install() {
        // Being unable to identify a card is not evidence that it is old.
        let odd = classify("NVIDIA Graphics Device");
        assert_eq!(odd, Generation::Unknown);
        assert!(odd.at_least(Generation::Blackwell));
    }

    #[test]
    fn generations_order_oldest_to_newest() {
        assert!(Generation::Blackwell > Generation::Ada);
        assert!(Generation::Ada > Generation::Ampere);
        assert!(Generation::Ampere > Generation::Turing);
        assert!(Generation::Turing > Generation::TuringNoRt);

        assert!(Generation::Ada.at_least(Generation::Turing));
        assert!(!Generation::Turing.at_least(Generation::Blackwell));
    }

    #[test]
    fn windows_driver_quads_translate_to_nvidia_versions() {
        assert_eq!(
            nvidia_driver_version("32.0.15.6109").as_deref(),
            Some("561.09")
        );
        assert_eq!(
            nvidia_driver_version("31.0.15.3742").as_deref(),
            Some("537.42")
        );
        // Not a version at all, and a quad with non-numeric parts.
        assert_eq!(nvidia_driver_version("hello"), None);
        assert_eq!(nvidia_driver_version("32.0.x.6109"), None);
        assert_eq!(nvidia_driver_version("32.0"), None);
    }

    #[test]
    fn a_non_nvidia_adapter_reports_no_nvidia_driver_version() {
        // The bug this guards, found against real hardware: the Intel iGPU on
        // the development machine reported "NVIDIA 188.61", because the quad
        // translation was applied to every vendor. A plausible-looking wrong
        // number is worse than a blank, because it reads as a fact.
        for adapter in adapters() {
            if matches!(
                adapter.generation,
                Generation::NotNvidia | Generation::Unknown
            ) {
                assert!(
                    adapter.nvidia_driver.is_none(),
                    "{} is not an NVIDIA card but reported NVIDIA driver {:?}",
                    adapter.name,
                    adapter.nvidia_driver
                );
            }
        }
    }

    #[test]
    fn reading_the_real_machine_never_panics() {
        // Runs at preflight time on whatever hardware this is, so asking must
        // be safe even on a CI runner with a basic display adapter.
        let found = adapters();
        for adapter in &found {
            assert!(!adapter.name.trim().is_empty());
        }
        // `best_nvidia` must agree with the list it filtered.
        if let Some(best) = best_nvidia() {
            assert!(found.iter().any(|adapter| adapter.name == best.name));
            assert_ne!(best.generation, Generation::NotNvidia);
        }
    }
}
