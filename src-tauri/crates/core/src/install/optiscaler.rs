//! Which `OptiScaler.ini` settings a given install needs.
//!
//! Pure: it takes what was decided elsewhere and returns the keys to write, so
//! the same situation always produces the same settings and the whole of it
//! can be tested without a game folder. Writing them is [`super::ini`]'s job.
//!
//! This exists because on this route the files are only half the install.
//! OptiScaler decides from its ini whether the neural pass runs, whether frame
//! generation runs, and - for a game with no DLSS - whether it hooks that
//! game's FSR or XeSS calls at all. Copy every file correctly, write no
//! settings, and the result is a component that loads and does nothing.
//!
//! # Only what the situation calls for
//!
//! Every key here is conditional. Nothing is written "to be safe", because a
//! key written into a user's tuned file is a change they did not ask for and
//! may not notice - and several of these are mutually exclusive answers to
//! "which upscaler are you hooking", where writing both is worse than writing
//! neither.
//!
//! Sourced from DLSS5-Autopilot's `optiscaler.py`, which drives the same
//! component.

use crate::scan::api::Direct3D;
use crate::scan::capability::{Feature, Substitute};
use crate::scan::integration::Upscaler;

/// One key to set, in one section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    pub section: &'static str,
    pub key: &'static str,
    pub value: String,
    /// Why, in one line, for the confirmation screen. A settings change is
    /// invisible in a file listing, so an install that writes four of them
    /// should be able to say what each was for.
    pub because: &'static str,
}

fn on(section: &'static str, key: &'static str, because: &'static str) -> Setting {
    Setting {
        section,
        key,
        value: "true".to_owned(),
        because,
    }
}

/// What the caller wants, beyond the situation itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// The neural pass's model resolution, as a percentage.
    ///
    /// `None` leaves whatever the file says, which is the right default for a
    /// user who has already tuned it. Autopilot's own default is 75: the cost
    /// falls with the square of this, so 75% is roughly half the cost of 100%
    /// and 50% a quarter, while the frame itself stays at full detail. It is
    /// the largest single lever on this route, which is why it is offered at
    /// all rather than left entirely to the in-game overlay.
    pub neural_scale: Option<u8>,
}

/// The model resolution as OptiScaler stores it: a fraction, to two places.
///
/// A percentage is what a person sets and a fraction is what the file holds -
/// `WorkingScale = 0.75`, not `75`. The first version of this wrote the
/// percentage under the key `Scale`, which is wrong twice over and silently:
/// OptiScaler ignores a key it does not know, so the dial would have stayed
/// where it was while the install reported having moved it. Reading a real
/// `OptiScaler.ini` is what caught it.
fn as_fraction(percent: u8) -> String {
    let value = f64::from(percent) / 100.0;
    let text = format!("{value:.2}");
    // `1.00` reads oddly beside the file's own `1.0`, and a trailing zero on
    // `0.50` is noise. Trimmed, but never past the point.
    let trimmed = text.trim_end_matches('0');
    if trimmed.ends_with('.') {
        format!("{trimmed}0")
    } else {
        trimmed.to_owned()
    }
}

/// The section OptiScaler reads its neural-rendering settings from.
///
/// An older release of Autopilot wrote this header in capitals, and its
/// current code rewrites `[DLSSNR]` to this spelling when it finds it. The
/// reader does not mind either way; two spellings in one file are what it
/// minds about. Our [`super::ini`] matches section names case-insensitively,
/// so a file carrying the old spelling is edited in place rather than gaining
/// a second section - which is the outcome that actually matters.
const NEURAL: &str = "DlssNr";

/// Keys that make OptiScaler treat a game's FSR calls as its input.
///
/// Both halves of each pair: `Enable*` lets it hook the entry points and
/// `Use*` makes it act on them. Writing one without the other is a hook that
/// reports success and changes nothing.
const FSR_INPUTS: [(&str, &str); 6] = [
    ("EnableFsr2Inputs", "hook the game's FSR 2 entry points"),
    ("UseFsr2Inputs", "and act on them"),
    ("EnableFsr3Inputs", "hook the game's FSR 3 entry points"),
    ("UseFsr3Inputs", "and act on them"),
    (
        "EnableFfxInputs",
        "hook the FidelityFX interface (FSR 2.3, 3.1, 4.x)",
    ),
    ("UseFfxInputs", "and act on them"),
];

/// The settings this install should write, in the order they are applied.
///
/// `upscaler` is the non-DLSS upscaler the game ships, if any;
/// `substitute` is the mechanism chosen for frame generation, if one was.
pub fn settings_for(
    wanted: &[Feature],
    upscaler: Option<Upscaler>,
    substitute: Option<Substitute>,
    direct3d: Option<Direct3D>,
    options: Options,
) -> Vec<Setting> {
    let mut found = Vec::new();

    if wanted.contains(&Feature::NeuralRendering) {
        found.push(on(
            NEURAL,
            "Enabled",
            "turns the neural rendering pass on; without it the runtime is loaded and idle",
        ));
        if let Some(scale) = options.neural_scale {
            found.push(Setting {
                section: NEURAL,
                key: "WorkingScale",
                value: as_fraction(scale),
                because: "the model's resolution - its cost falls with the square of this, and \
                          the frame itself stays at full detail",
            });
        }
    }

    // Frame generation, when it is the substitute that delivers it. Not when
    // the game drives DLSS frame generation itself: that is a better result
    // and turning FSR's on beside it would be two frame generators arguing.
    if substitute == Some(Substitute::FsrFrameGeneration) {
        found.push(on(
            "FrameGen",
            "Enabled",
            "turns AMD FSR 3.1 frame generation on",
        ));
        found.push(Setting {
            section: "FrameGen",
            key: "FGInput",
            value: "upscaler".to_owned(),
            because: "feeds it from the upscaler this route is already running",
        });
        found.push(Setting {
            section: "FrameGen",
            key: "FGOutput",
            value: "fsrfg".to_owned(),
            because: "and sends the result to FSR's frame generator",
        });
        // Not optional with the upscaler as input: without it the interface is
        // generated along with the frame and text ghosts.
        found.push(on(
            "OptiFG",
            "HUDFix",
            "keeps the interface out of the generated frame - without it text ghosts",
        ));
    }

    // Taking over a game's own FSR or XeSS. Only for a game that has one to
    // take over: on a game with DLSS these keys hook entry points nothing
    // calls.
    match upscaler {
        Some(Upscaler::Fsr) => {
            for (key, because) in FSR_INPUTS {
                found.push(on("Inputs", key, because));
            }
            // A Direct3D 11 game calls the D3D11 FSR2 entry points, which
            // OptiScaler hooks in place of the D3D12 ones only when told to.
            if direct3d == Some(Direct3D::Eleven) {
                found.push(on(
                    "Inputs",
                    "UseFsr2Dx11Inputs",
                    "this game calls FSR's Direct3D 11 entry points rather than its Direct3D 12 \
                     ones",
                ));
            }
        }
        Some(Upscaler::Xess) => found.push(on(
            "Inputs",
            "EnableXeSSInputs",
            "hook the game's XeSS entry points",
        )),
        None => {}
    }

    // Which upscaler OptiScaler runs, once it has the inputs.
    //
    // Two different keys for two different jobs, and the conditions do not
    // overlap. On Direct3D 12 a redirected game should end up in DLSS, which
    // is the point of redirecting it. On Direct3D 11 the neural model refuses
    // to run at all, so OptiScaler carries it on its own Direct3D 12 bridge -
    // and DLSS cannot be the upscaler there, which is why this names FSR.
    if direct3d == Some(Direct3D::Eleven) {
        if wanted.contains(&Feature::NeuralRendering) {
            found.push(Setting {
                section: "Upscalers",
                key: "Dx11Upscaler",
                value: "fsr22_12".to_owned(),
                because: "the neural model does not run on Direct3D 11, so FSR 2.2 on \
                          OptiScaler's Direct3D 12 bridge carries it",
            });
        }
    } else if upscaler.is_some() {
        found.push(Setting {
            section: "Upscalers",
            key: "Dx12Upscaler",
            value: "dlss".to_owned(),
            because: "run DLSS in place of the calls being redirected",
        });
    }

    found
}

/// Applies `settings` to an ini's text.
///
/// Grouped by section so each one is opened once, which keeps the edit to the
/// smallest number of passes and makes the result independent of the order the
/// settings happen to be in.
pub fn write_into(text: &str, settings: &[Setting]) -> String {
    let mut out = text.to_owned();
    let mut done: Vec<&str> = Vec::new();

    for setting in settings {
        if done.contains(&setting.section) {
            continue;
        }
        done.push(setting.section);

        let values: Vec<(&str, &str)> = settings
            .iter()
            .filter(|other| other.section == setting.section)
            .map(|other| (other.key, other.value.as_str()))
            .collect();
        out = super::ini::set(&out, setting.section, &values);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(settings: &[Setting]) -> Vec<(&str, &str, &str)> {
        settings
            .iter()
            .map(|item| (item.section, item.key, item.value.as_str()))
            .collect()
    }

    #[test]
    fn a_plain_neural_install_writes_one_key() {
        // A game with its own DLSS on Direct3D 12: nothing to redirect, no
        // substitute, so the only thing the file has to say is that the pass
        // is on. Everything else stays as the user left it.
        let found = settings_for(
            &[Feature::NeuralRendering],
            None,
            None,
            Some(Direct3D::Twelve),
            Options::default(),
        );
        assert_eq!(keys(&found), vec![("DlssNr", "Enabled", "true")]);
    }

    #[test]
    fn the_resolution_dial_is_left_alone_unless_asked_for() {
        // It is the biggest lever on this route and it is also a preference.
        // Writing a default over someone's tuned value is the kind of help
        // that is indistinguishable from a bug.
        let untouched = settings_for(
            &[Feature::NeuralRendering],
            None,
            None,
            Some(Direct3D::Twelve),
            Options::default(),
        );
        assert!(untouched.iter().all(|item| item.key != "Scale"));

        let asked = settings_for(
            &[Feature::NeuralRendering],
            None,
            None,
            Some(Direct3D::Twelve),
            Options {
                neural_scale: Some(75),
            },
        );
        assert!(asked
            .iter()
            .any(|item| item.key == "WorkingScale" && item.value == "0.75"));
    }

    #[test]
    fn the_dial_is_written_the_way_the_file_holds_it() {
        // Found by reading a real OptiScaler.ini rather than by reasoning: the
        // key is WorkingScale and the value is a fraction. The first version
        // wrote `Scale = 75`, which is wrong twice and silently - OptiScaler
        // ignores a key it does not know, so the dial stays where it was while
        // the install reports having moved it.
        assert_eq!(as_fraction(75), "0.75");
        assert_eq!(as_fraction(50), "0.5");
        assert_eq!(as_fraction(100), "1.0");
        assert_eq!(as_fraction(25), "0.25");

        let found = settings_for(
            &[Feature::NeuralRendering],
            None,
            None,
            Some(Direct3D::Twelve),
            Options {
                neural_scale: Some(60),
            },
        );
        let dial = found
            .iter()
            .find(|item| item.section == "DlssNr" && item.key == "WorkingScale")
            .expect("the dial is written under the name the file uses");
        assert_eq!(dial.value, "0.6");
        assert!(
            found.iter().all(|item| item.key != "Scale"),
            "the key OptiScaler would ignore must not be written"
        );
    }

    #[test]
    fn frame_generation_is_written_only_when_it_is_the_substitute() {
        // A game that drives DLSS frame generation itself gets a better
        // result than this, and turning FSR's on beside it would be two frame
        // generators arguing over the same present.
        let native = settings_for(
            &[Feature::FrameGeneration],
            None,
            None,
            Some(Direct3D::Twelve),
            Options::default(),
        );
        assert!(native.is_empty(), "{native:?}");

        let substituted = settings_for(
            &[Feature::FrameGeneration],
            None,
            Some(Substitute::FsrFrameGeneration),
            Some(Direct3D::Twelve),
            Options::default(),
        );
        assert_eq!(
            keys(&substituted),
            vec![
                ("FrameGen", "Enabled", "true"),
                ("FrameGen", "FGInput", "upscaler"),
                ("FrameGen", "FGOutput", "fsrfg"),
                ("OptiFG", "HUDFix", "true"),
            ]
        );
    }

    #[test]
    fn hudfix_is_not_optional() {
        // OptiScaler's own note: with the upscaler as input and no HUDFix, the
        // interface is generated along with the frame and text ghosts. It is
        // part of turning frame generation on rather than a preference.
        let found = settings_for(
            &[Feature::FrameGeneration],
            None,
            Some(Substitute::FsrFrameGeneration),
            Some(Direct3D::Twelve),
            Options::default(),
        );
        assert!(found
            .iter()
            .any(|item| item.section == "OptiFG" && item.key == "HUDFix"));
    }

    #[test]
    fn both_halves_of_each_input_pair_are_written() {
        // Enable* hooks the entry point and Use* acts on it. Writing one
        // without the other is a hook that reports success and does nothing,
        // which is the silent failure this whole application is about.
        let found = settings_for(
            &[Feature::NeuralRendering],
            Some(Upscaler::Fsr),
            None,
            Some(Direct3D::Twelve),
            Options::default(),
        );
        for pair in ["Fsr2", "Fsr3", "Ffx"] {
            assert!(
                found
                    .iter()
                    .any(|item| item.key == format!("Enable{pair}Inputs")),
                "missing Enable{pair}Inputs"
            );
            assert!(
                found
                    .iter()
                    .any(|item| item.key == format!("Use{pair}Inputs")),
                "missing Use{pair}Inputs"
            );
        }
    }

    #[test]
    fn a_redirected_game_is_told_to_run_dlss() {
        // Redirecting FSR into OptiScaler and leaving the upscaler on auto
        // would be most of the work for none of the benefit.
        let found = settings_for(
            &[Feature::NeuralRendering],
            Some(Upscaler::Xess),
            None,
            Some(Direct3D::Twelve),
            Options::default(),
        );
        assert!(found
            .iter()
            .any(|item| item.key == "Dx12Upscaler" && item.value == "dlss"));
        assert!(found.iter().any(|item| item.key == "EnableXeSSInputs"));
    }

    #[test]
    fn a_game_with_dlss_is_not_told_to_hook_anything() {
        // These keys hook entry points a DLSS game never calls, and
        // Dx12Upscaler would be answering a question nobody asked.
        let found = settings_for(
            &[Feature::NeuralRendering],
            None,
            None,
            Some(Direct3D::Twelve),
            Options::default(),
        );
        assert!(found.iter().all(|item| item.section != "Inputs"));
        assert!(found.iter().all(|item| item.key != "Dx12Upscaler"));
    }

    #[test]
    fn direct3d_11_gets_the_bridge_upscaler_and_the_dx11_hooks() {
        // Two separate consequences of the same fact. The model refuses to run
        // on D3D11, so OptiScaler carries it on its own D3D12 bridge and DLSS
        // cannot be the upscaler there. And an FSR game on D3D11 calls the
        // D3D11 entry points, which are hooked only when asked for.
        let found = settings_for(
            &[Feature::NeuralRendering],
            Some(Upscaler::Fsr),
            None,
            Some(Direct3D::Eleven),
            Options::default(),
        );
        assert!(found
            .iter()
            .any(|item| item.key == "Dx11Upscaler" && item.value == "fsr22_12"));
        assert!(found.iter().any(|item| item.key == "UseFsr2Dx11Inputs"));
        // And not the D3D12 answer, which would be the wrong one here.
        assert!(found.iter().all(|item| item.key != "Dx12Upscaler"));
    }

    #[test]
    fn nothing_wanted_writes_nothing() {
        assert!(
            settings_for(&[], None, None, Some(Direct3D::Twelve), Options::default()).is_empty()
        );
    }

    #[test]
    fn writing_them_touches_each_section_once() {
        let settings = settings_for(
            &[Feature::NeuralRendering, Feature::FrameGeneration],
            Some(Upscaler::Fsr),
            Some(Substitute::FsrFrameGeneration),
            Some(Direct3D::Twelve),
            Options::default(),
        );
        let text = write_into("", &settings);

        // One header per section, however many keys went into it.
        for section in ["DlssNr", "FrameGen", "OptiFG", "Inputs", "Upscalers"] {
            assert_eq!(
                text.matches(&format!("[{section}]")).count(),
                1,
                "{section} in {text}"
            );
        }
        // And every setting is in there.
        for setting in &settings {
            assert!(
                text.contains(&format!("{}={}", setting.key, setting.value)),
                "{} missing from {text}",
                setting.key
            );
        }
    }

    #[test]
    fn writing_them_twice_changes_nothing_the_second_time() {
        let settings = settings_for(
            &[Feature::NeuralRendering],
            Some(Upscaler::Fsr),
            Some(Substitute::FsrFrameGeneration),
            Some(Direct3D::Twelve),
            Options::default(),
        );
        let once = write_into("", &settings);
        assert_eq!(once, write_into(&once, &settings));
    }

    #[test]
    fn an_existing_section_in_the_old_spelling_is_not_duplicated() {
        // Autopilot's older releases wrote `[DLSSNR]`. Matching case
        // insensitively means such a file is edited in place; matching exactly
        // would leave two neural sections and let the reader pick.
        let before = "[DLSSNR]\r\nEnabled = false\r\n";
        let settings = settings_for(
            &[Feature::NeuralRendering],
            None,
            None,
            Some(Direct3D::Twelve),
            Options::default(),
        );
        let after = write_into(before, &settings);

        assert_eq!(
            after.to_lowercase().matches("[dlssnr]").count(),
            1,
            "{after}"
        );
        assert!(after.contains("Enabled = true"), "{after}");
    }

    #[test]
    fn every_setting_says_why_it_is_there() {
        // A settings change leaves no trace in a file listing, so an install
        // that writes eleven of them has to be able to account for each.
        let settings = settings_for(
            &[Feature::NeuralRendering, Feature::FrameGeneration],
            Some(Upscaler::Fsr),
            Some(Substitute::FsrFrameGeneration),
            Some(Direct3D::Eleven),
            Options {
                neural_scale: Some(75),
            },
        );
        assert!(!settings.is_empty());
        for setting in &settings {
            assert!(!setting.because.is_empty(), "{setting:?}");
        }
    }
}
