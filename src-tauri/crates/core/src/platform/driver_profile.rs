//! Reading NVIDIA's per-game driver settings, so an install can tell when the
//! driver is going to override it.
//!
//! # Why this exists
//!
//! The NVIDIA App has a "DLSS Override" feature. When it is on for a game, the
//! driver substitutes its own runtime from the NGX store - see
//! [`crate::install::discover::from_ngx_store`] - **regardless of the file in
//! the game folder**. So a user can swap in 310.8.0, launch, get the driver's
//! 310.7.129 instead, and see no error anywhere. Every file operation
//! succeeded; the result is simply not what the tool reported.
//!
//! That is the worst class of failure this project is trying to remove, and it
//! is invisible without reading driver state. Hence this module.
//!
//! # This module never writes
//!
//! There is no set, save, delete or restore here, and the omission is
//! deliberate rather than unfinished. Driver profiles are machine-wide state
//! that affects every application, the NVIDIA App is their owner, and a tool
//! that silently rewrote them would be doing something the user did not ask
//! for and cannot easily see. Reading tells us what to *warn* about, which is
//! the whole requirement.
//!
//! Writing them would also be the lowest-overhead install route there is - no
//! files in the game folder at all - so this may well grow a write half. That
//! is a decision to take deliberately, with the user asked first, not a
//! capability to acquire by accident.
//!
//! # How it works
//!
//! `nvapi64.dll` exports exactly one symbol, `nvapi_QueryInterface`, which maps
//! a numeric id to a function pointer. The ids come from NVIDIA's public
//! `nvapi_interface.h`.
//!
//! `NVDRS_SETTING` is a 12320-byte structure whose layout can be derived rather
//! than guessed:
//!
//! ```text
//! offset  field
//!      0  version                 = size | (1 << 16)
//!      4  settingName             NvU16[2048]
//!   4100  settingId
//!   4104  settingType             0 = DWORD
//!   4108  settingLocation
//!   4112  isCurrentPredefined     0 when the user has set it
//!   4116  isPredefinedValid
//!   4120  predefinedValue         union, 4100 bytes
//!   8220  currentValue            union, 4100 bytes
//!  12320  end
//! ```
//!
//! The union is `NVDRS_BINARY_SETTING`, which is the largest arm at
//! `valueLength` plus `valueData[4096]`. 4120 + 4100 = 8220, and 8220 + 4100 =
//! 12320, which agrees with the size a working third-party implementation uses.
//! Two independent derivations landing on the same numbers is the reason this
//! is written down.
//!
//! Every call degrades to `None`. A machine with no NVIDIA driver has no
//! `nvapi64.dll`, and that is a normal answer rather than an error.

use serde::{Deserialize, Serialize};

/// A driver setting worth knowing about before installing.
///
/// The ids are the NVIDIA App's own, and the four `LatestDll` ones are what
/// "DLSS Override" writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Setting {
    /// Force the driver's own super resolution runtime.
    SuperResolutionLatestDll,
    /// Force the driver's own ray reconstruction runtime.
    RayReconstructionLatestDll,
    /// Force the driver's own frame generation runtime.
    FrameGenerationLatestDll,
    /// Force the driver's own neural rendering runtime.
    NeuralRenderingLatestDll,
    /// Force a super resolution render preset.
    SuperResolutionPreset,
    /// Force a ray reconstruction render preset.
    RayReconstructionPreset,
    /// Force a frame generation render preset.
    FrameGenerationPreset,
    /// Force a neural rendering render preset.
    NeuralRenderingPreset,
}

impl Setting {
    pub const ALL: [Setting; 8] = [
        Setting::SuperResolutionLatestDll,
        Setting::RayReconstructionLatestDll,
        Setting::FrameGenerationLatestDll,
        Setting::NeuralRenderingLatestDll,
        Setting::SuperResolutionPreset,
        Setting::RayReconstructionPreset,
        Setting::FrameGenerationPreset,
        Setting::NeuralRenderingPreset,
    ];

    pub const fn id(self) -> u32 {
        match self {
            Setting::SuperResolutionLatestDll => 0x10E4_1E01,
            Setting::RayReconstructionLatestDll => 0x10E4_1E02,
            Setting::FrameGenerationLatestDll => 0x10E4_1E03,
            Setting::NeuralRenderingLatestDll => 0x10E4_1E04,
            Setting::SuperResolutionPreset => 0x10E4_1DF3,
            Setting::RayReconstructionPreset => 0x10E4_1DF7,
            Setting::FrameGenerationPreset => 0x10E4_1DF1,
            Setting::NeuralRenderingPreset => 0x10E4_1DF8,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Setting::SuperResolutionLatestDll => "DLSS Override - Super Resolution runtime",
            Setting::RayReconstructionLatestDll => "DLSS Override - Ray Reconstruction runtime",
            Setting::FrameGenerationLatestDll => "DLSS Override - Frame Generation runtime",
            Setting::NeuralRenderingLatestDll => "DLSS Override - Neural Rendering runtime",
            Setting::SuperResolutionPreset => "DLSS Override - Super Resolution preset",
            Setting::RayReconstructionPreset => "DLSS Override - Ray Reconstruction preset",
            Setting::FrameGenerationPreset => "DLSS Override - Frame Generation preset",
            Setting::NeuralRenderingPreset => "DLSS Override - Neural Rendering preset",
        }
    }

    /// Which feature's runtime this setting would displace, if any.
    ///
    /// Only the `LatestDll` settings replace a file. A preset override changes
    /// how the runtime behaves without changing which runtime loads, so it is
    /// worth reporting but does not invalidate an install.
    pub const fn displaces_runtime(self) -> Option<crate::scan::capability::Feature> {
        use crate::scan::capability::Feature;
        match self {
            Setting::SuperResolutionLatestDll => Some(Feature::SuperResolution),
            Setting::RayReconstructionLatestDll => Some(Feature::RayReconstruction),
            Setting::FrameGenerationLatestDll => Some(Feature::FrameGeneration),
            Setting::NeuralRenderingLatestDll => Some(Feature::NeuralRendering),
            _ => None,
        }
    }
}

/// What the driver has recorded for one setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Value {
    pub setting: Setting,
    pub value: u32,
    /// True when this is the driver's own default, false when somebody set it.
    ///
    /// The distinction matters for what we say: a default is background, a
    /// value the user chose in the NVIDIA App is a decision that our install
    /// is about to fight with.
    pub chosen_by_user: bool,
}

impl Value {
    /// Whether this value means the driver will ignore a swapped-in runtime.
    ///
    /// Zero is off for every one of these settings. A non-zero `LatestDll`
    /// means the driver supplies the runtime itself.
    pub fn overrides_our_install(&self) -> bool {
        self.value != 0 && self.setting.displaces_runtime().is_some()
    }
}

/// Everything read for one game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    /// The profile the driver matched, when it named one.
    pub name: Option<String>,
    pub values: Vec<Value>,
}

impl Profile {
    /// The features whose install would be silently overridden.
    pub fn overridden(&self) -> Vec<crate::scan::capability::Feature> {
        let mut found: Vec<_> = self
            .values
            .iter()
            .filter(|value| value.overrides_our_install())
            .filter_map(|value| value.setting.displaces_runtime())
            .collect();
        found.sort_unstable();
        found.dedup();
        found
    }

    /// A sentence for the user, or `None` when there is nothing to say.
    pub fn warning(&self) -> Option<String> {
        let overridden = self.overridden();
        if overridden.is_empty() {
            return None;
        }
        let names: Vec<&str> = overridden
            .iter()
            .map(|feature| feature.label())
            .collect::<Vec<_>>();
        Some(format!(
            "Your NVIDIA driver is set to supply {} itself for this game, so it will use its \
             own runtime and ignore the one installed here. Turn DLSS Override off for this \
             game in the NVIDIA app to let this install take effect.",
            join(&names)
        ))
    }
}

fn join(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Read the global profile's DLSS settings.
///
/// The base profile applies to everything without a profile of its own, so a
/// DLSS Override set there affects every game.
pub fn global() -> Option<Profile> {
    imp::read(None)
}

/// Read the settings that apply to one executable.
///
/// `exe_name` is a bare file name such as `Cyberpunk2077.exe`. The driver
/// matches profiles by executable, and the match is the driver's own - which
/// is the point, since it is the driver's opinion that decides what loads.
pub fn for_executable(exe_name: &str) -> Option<Profile> {
    imp::read(Some(exe_name))
}

#[cfg(windows)]
mod imp {
    use super::{Profile, Setting, Value};

    /// Function ids from NVIDIA's public `nvapi_interface.h`.
    ///
    /// One cross-check worth recording: a working third-party implementation
    /// uses `0xFA5B6166` for `RestoreProfileDefault` where the published table
    /// says `0xFA5F6134`. This module needs neither, but the disagreement is
    /// the reason every id here comes from the published table rather than
    /// from reading someone else's constants.
    mod id {
        pub const INITIALIZE: u32 = 0x0150_E828;
        pub const DRS_CREATE_SESSION: u32 = 0x0694_D52E;
        pub const DRS_DESTROY_SESSION: u32 = 0xDAD9_CFF8;
        pub const DRS_LOAD_SETTINGS: u32 = 0x375D_BD6B;
        pub const DRS_GET_BASE_PROFILE: u32 = 0xDA84_66A0;
        pub const DRS_FIND_APPLICATION_BY_NAME: u32 = 0xEEE5_66B2;
        pub const DRS_GET_PROFILE_INFO: u32 = 0x61CD_6FD6;
        /// The published id. Newer drivers also expose an undocumented
        /// variant, tried first by [`get_setting_fn`], because on some driver
        /// branches the published one no longer resolves.
        pub const DRS_GET_SETTING: u32 = 0x73BF_8338;
        pub const DRS_GET_SETTING_R610: u32 = 0xEA99_498D;
    }

    /// `NVDRS_SETTING`. See the module documentation for the derivation.
    mod setting_layout {
        pub const SIZE: usize = 12320;
        pub const VERSION: u32 = SIZE as u32 | (1 << 16);
        pub const SETTING_TYPE: usize = 4104;
        pub const IS_CURRENT_PREDEFINED: usize = 4112;
        pub const CURRENT_VALUE: usize = 8220;
        /// `NVDRS_DWORD_TYPE`. Any other type means `currentValue` is not a
        /// `u32` and reading it as one would report a number that is not the
        /// setting's value.
        pub const DWORD_TYPE: u32 = 0;
    }

    /// `NVDRS_PROFILE`, read only for its name.
    ///
    /// `version | numOfApps | numOfSettings | gpuSupport[?]` after a 4096-byte
    /// name. Only the name is used, and only the versions the driver accepts
    /// are tried, so a layout change shows up as "no name" rather than as a
    /// wrong one.
    mod profile_layout {
        pub const NAME: usize = 4;
        pub const NAME_BYTES: usize = 4096;
        /// Measured, not derived. The driver accepts exactly 4116 and
        /// answers `-9` (incompatible struct version) for 4112 or 4120, so
        /// V1 is `version + name + gpuSupport + isPredefined + numOfApps +
        /// numOfSettings`. An earlier count here was one field short; the
        /// driver rejected it and the name simply never came back.
        pub const SIZE_V1: usize = 4 + NAME_BYTES + 4 + 4 + 4 + 4;
    }

    type QueryInterface = unsafe extern "C" fn(u32) -> *const ();
    type Simple = unsafe extern "C" fn() -> i32;
    type OneHandle = unsafe extern "C" fn(*mut ()) -> i32;
    type CreateSession = unsafe extern "C" fn(*mut *mut ()) -> i32;
    type GetBaseProfile = unsafe extern "C" fn(*mut (), *mut *mut ()) -> i32;
    type GetProfileInfo = unsafe extern "C" fn(*mut (), *mut (), *mut u8) -> i32;
    type FindApplication = unsafe extern "C" fn(*mut (), *const u16, *mut *mut (), *mut u8) -> i32;
    /// The published `NvAPI_DRS_GetSetting`.
    type GetSetting = unsafe extern "C" fn(*mut (), *mut (), u32, *mut u8) -> i32;
    /// The R610+ variant, which takes one more out-parameter.
    type GetSettingR610 = unsafe extern "C" fn(*mut (), *mut (), u32, *mut u8, *mut u32) -> i32;

    /// Whichever `GetSetting` this driver branch exposes, with the signature
    /// that id actually has.
    ///
    /// The two are **not** interchangeable, and assuming they were cost a
    /// morning. `0xEA99498D` takes a fifth out-parameter; called through the
    /// four-argument signature it writes through whatever happens to be in
    /// that register, which faults immediately and looks for all the world
    /// like a bad struct layout.
    #[derive(Clone, Copy)]
    enum SettingReader {
        Published(GetSetting),
        R610(GetSettingR610),
    }

    impl SettingReader {
        /// SAFETY: `session` and `profile` must be live handles from this
        /// session, and `buffer` must be a `NVDRS_SETTING` of the size its
        /// version field declares.
        #[allow(unsafe_code)]
        unsafe fn get(self, session: *mut (), profile: *mut (), id: u32, buffer: *mut u8) -> i32 {
            match self {
                SettingReader::Published(get) => unsafe { get(session, profile, id, buffer) },
                SettingReader::R610(get) => {
                    let mut extra: u32 = 0;
                    unsafe { get(session, profile, id, buffer, &raw mut extra) }
                }
            }
        }
    }

    /// The loaded library and its one export.
    ///
    /// Held for the life of the process rather than freed after each read: the
    /// driver's session machinery is not documented as safe to unload and
    /// reload repeatedly, and the library is a few hundred kilobytes.
    #[allow(unsafe_code)]
    fn query_interface() -> Option<QueryInterface> {
        use std::sync::OnceLock;
        static LOADED: OnceLock<Option<usize>> = OnceLock::new();

        let address = (*LOADED.get_or_init(|| {
            use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
            let name: Vec<u16> = "nvapi64.dll\0".encode_utf16().collect();
            // SAFETY: `name` is NUL-terminated and outlives the call. A
            // machine without the NVIDIA driver returns null, which is the
            // expected answer rather than a failure.
            let module = unsafe { LoadLibraryW(name.as_ptr()) };
            if module.is_null() {
                return None;
            }
            // SAFETY: `module` is a live handle from the call above, and the
            // symbol name is a NUL-terminated byte string. `nvapi_QueryInterface`
            // is the only export nvapi64.dll has.
            let symbol = unsafe { GetProcAddress(module, c"nvapi_QueryInterface".as_ptr().cast()) };
            symbol.map(|found| found as usize)
        }))?;

        // SAFETY: the address came from `GetProcAddress` for
        // `nvapi_QueryInterface`, whose signature is `void*(unsigned int)`.
        Some(unsafe { std::mem::transmute::<usize, QueryInterface>(address) })
    }

    /// Resolve one function, or `None` when this driver does not have it.
    #[allow(unsafe_code)]
    fn resolve(id: u32) -> Option<*const ()> {
        let query = query_interface()?;
        // SAFETY: calling the resolved export with an interface id, which is
        // what it takes. An unknown id returns null.
        let found = unsafe { query(id) };
        (!found.is_null()).then_some(found)
    }

    /// `GetSetting`, preferring the published id.
    ///
    /// The published one is tried first because its signature is the
    /// documented one. The R610 variant is the fallback for driver branches
    /// where the published id no longer resolves.
    #[allow(unsafe_code)]
    fn get_setting_fn() -> Option<SettingReader> {
        if let Some(address) = resolve(id::DRS_GET_SETTING) {
            // SAFETY: this id names the published `NvAPI_DRS_GetSetting`,
            // which is `(session, profile, settingId, NVDRS_SETTING*)`.
            return Some(SettingReader::Published(unsafe {
                std::mem::transmute::<*const (), GetSetting>(address)
            }));
        }
        let address = resolve(id::DRS_GET_SETTING_R610)?;
        // SAFETY: this id takes the same four arguments plus one `NvU32`
        // out-parameter, which `SettingReader::get` supplies.
        Some(SettingReader::R610(unsafe {
            std::mem::transmute::<*const (), GetSettingR610>(address)
        }))
    }

    /// A session, closed when dropped.
    ///
    /// The guard exists so that every early return below - and there are many,
    /// because every step can fail on a machine we know nothing about - still
    /// destroys the session and unloads the library.
    struct Session {
        handle: *mut (),
    }

    impl Drop for Session {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            if let Some(address) = resolve(id::DRS_DESTROY_SESSION) {
                // SAFETY: `handle` came from `NvAPI_DRS_CreateSession` and has
                // not been destroyed - this runs once, from `Drop`.
                let destroy: OneHandle =
                    unsafe { std::mem::transmute::<*const (), OneHandle>(address) };
                unsafe { destroy(self.handle) };
            }
            // Deliberately no `NvAPI_Unload` here.
            //
            // Pairing every read with an unload looks tidy and crashes on the
            // second read. `NvAPI_Unload` can drop the library out of the
            // process, while `query_interface` caches the address of
            // `nvapi_QueryInterface` for the process lifetime - so the next
            // call jumps through a stale pointer. It reads as a mysterious
            // access violation in whichever call happens to come first.
            //
            // NvAPI is designed to be initialised once per process, so it is,
            // by `initialize` below, and left that way.
        }
    }

    /// `NvAPI_Initialize`, once per process.
    #[allow(unsafe_code)]
    fn initialize() -> Option<()> {
        use std::sync::OnceLock;
        static READY: OnceLock<bool> = OnceLock::new();

        let ready = *READY.get_or_init(|| {
            let Some(address) = resolve(id::INITIALIZE) else {
                return false;
            };
            // SAFETY: `NvAPI_Initialize` takes no arguments and returns a
            // status code.
            let initialize: Simple = unsafe { std::mem::transmute::<*const (), Simple>(address) };
            // SAFETY: no arguments to get wrong.
            unsafe { initialize() == 0 }
        });
        ready.then_some(())
    }

    #[allow(unsafe_code)]
    pub(super) fn read(exe_name: Option<&str>) -> Option<Profile> {
        initialize()?;

        let create = resolve(id::DRS_CREATE_SESSION)?;
        // SAFETY: `NvAPI_DRS_CreateSession` writes one handle through its
        // out-parameter.
        let create: CreateSession =
            unsafe { std::mem::transmute::<*const (), CreateSession>(create) };
        let mut handle: *mut () = std::ptr::null_mut();
        // SAFETY: `&raw mut handle` is a live local for the duration.
        if unsafe { create(&raw mut handle) } != 0 || handle.is_null() {
            return None;
        }
        // From here on every exit destroys the session.
        let session = Session { handle };

        let load = resolve(id::DRS_LOAD_SETTINGS)?;
        // SAFETY: `NvAPI_DRS_LoadSettings` takes the session handle.
        let load: OneHandle = unsafe { std::mem::transmute::<*const (), OneHandle>(load) };
        // SAFETY: `session.handle` is live and was created above.
        if unsafe { load(session.handle) } != 0 {
            return None;
        }

        let (profile, name) = match exe_name {
            Some(exe) => find_application(&session, exe)?,
            None => (base_profile(&session)?, None),
        };

        let get = get_setting_fn()?;
        let mut values = Vec::new();
        for setting in Setting::ALL {
            if let Some(value) = read_one(get, &session, profile, setting) {
                values.push(value);
            }
        }
        Some(Profile { name, values })
    }

    #[allow(unsafe_code)]
    fn base_profile(session: &Session) -> Option<*mut ()> {
        let address = resolve(id::DRS_GET_BASE_PROFILE)?;
        // SAFETY: `NvAPI_DRS_GetBaseProfile(session, *profile)`.
        let get: GetBaseProfile =
            unsafe { std::mem::transmute::<*const (), GetBaseProfile>(address) };
        let mut profile: *mut () = std::ptr::null_mut();
        // SAFETY: the session is live; the out-pointer is a live local.
        if unsafe { get(session.handle, &raw mut profile) } != 0 || profile.is_null() {
            return None;
        }
        Some(profile)
    }

    /// Find the profile the driver would apply to one executable.
    ///
    /// `NVDRS_APPLICATION` has grown four times, and each version stamps its
    /// own size. Rather than guess which one this driver wants, every known
    /// size is offered in turn: NvAPI validates the version field and returns
    /// an "incompatible struct version" status for one it does not accept, so
    /// a wrong guess is a rejected call rather than a misread structure.
    #[allow(unsafe_code)]
    fn find_application(session: &Session, exe_name: &str) -> Option<(*mut (), Option<String>)> {
        const UNICODE_BYTES: usize = 4096;
        // v1: version, isPredefined, appName, userFriendlyName, launcher
        const V1: usize = 4 + 4 + UNICODE_BYTES * 3;
        // v2 adds fileInFolder
        const V2: usize = V1 + UNICODE_BYTES;
        // v3 adds a bitfield word
        const V3: usize = V2 + 4;
        // v4 adds commandLine
        const V4: usize = V3 + UNICODE_BYTES;

        let address = resolve(id::DRS_FIND_APPLICATION_BY_NAME)?;
        // SAFETY: `NvAPI_DRS_FindApplicationByName(session, name, *profile,
        // NVDRS_APPLICATION*)`.
        let find: FindApplication =
            unsafe { std::mem::transmute::<*const (), FindApplication>(address) };

        let wide: Vec<u16> = exe_name.encode_utf16().chain(std::iter::once(0)).collect();

        for (size, version) in [(V4, 4u32), (V3, 3), (V2, 2), (V1, 1)] {
            let mut buffer = vec![0u8; size];
            buffer[..4].copy_from_slice(&(size as u32 | (version << 16)).to_le_bytes());
            let mut profile: *mut () = std::ptr::null_mut();
            // SAFETY: `wide` is NUL-terminated and outlives the call;
            // `buffer` is `size` bytes and its version field declares exactly
            // that size, which is the contract NvAPI checks. Both pointers are
            // live locals.
            let status = unsafe {
                find(
                    session.handle,
                    wide.as_ptr(),
                    &raw mut profile,
                    buffer.as_mut_ptr(),
                )
            };
            if status == 0 && !profile.is_null() {
                return Some((profile, profile_name(session, profile)));
            }
        }
        None
    }

    /// The profile's name, for reporting. Absence is not an error.
    #[allow(unsafe_code)]
    fn profile_name(session: &Session, profile: *mut ()) -> Option<String> {
        let address = resolve(id::DRS_GET_PROFILE_INFO)?;
        // SAFETY: `NvAPI_DRS_GetProfileInfo(session, profile, NVDRS_PROFILE*)`.
        let get: GetProfileInfo =
            unsafe { std::mem::transmute::<*const (), GetProfileInfo>(address) };

        let mut buffer = vec![0u8; profile_layout::SIZE_V1];
        buffer[..4].copy_from_slice(&(profile_layout::SIZE_V1 as u32 | (1 << 16)).to_le_bytes());
        // SAFETY: the session and profile handles are live, and `buffer` is
        // the number of bytes its version field declares.
        if unsafe { get(session.handle, profile, buffer.as_mut_ptr()) } != 0 {
            return None;
        }
        let name = &buffer[profile_layout::NAME..profile_layout::NAME + profile_layout::NAME_BYTES];
        let wide: Vec<u16> = name
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|unit| *unit != 0)
            .collect();
        (!wide.is_empty()).then(|| String::from_utf16_lossy(&wide))
    }

    #[allow(unsafe_code)]
    fn read_one(
        get: SettingReader,
        session: &Session,
        profile: *mut (),
        setting: Setting,
    ) -> Option<Value> {
        let mut buffer = vec![0u8; setting_layout::SIZE];
        buffer[..4].copy_from_slice(&setting_layout::VERSION.to_le_bytes());

        // SAFETY: both handles are live, and `buffer` is exactly the
        // `NVDRS_SETTING` size its version field declares. A setting the
        // profile does not carry returns a non-zero status and leaves the
        // buffer as it was.
        if unsafe { get.get(session.handle, profile, setting.id(), buffer.as_mut_ptr()) } != 0 {
            return None;
        }

        // Only a DWORD setting has a `u32` at `currentValue`. Reading another
        // type as one would report a number that is not the setting's value,
        // which is worse than reporting nothing.
        if read_u32(&buffer, setting_layout::SETTING_TYPE)? != setting_layout::DWORD_TYPE {
            return None;
        }

        Some(Value {
            setting,
            value: read_u32(&buffer, setting_layout::CURRENT_VALUE)?,
            chosen_by_user: read_u32(&buffer, setting_layout::IS_CURRENT_PREDEFINED)? == 0,
        })
    }

    fn read_u32(buffer: &[u8], offset: usize) -> Option<u32> {
        let bytes = buffer.get(offset..offset + 4)?;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

#[cfg(not(windows))]
mod imp {
    pub(super) fn read(_exe_name: Option<&str>) -> Option<super::Profile> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::capability::Feature;

    #[test]
    fn every_setting_has_a_distinct_id_and_a_label() {
        let mut ids: Vec<u32> = Setting::ALL.iter().map(|setting| setting.id()).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "two settings share an id");
        for setting in Setting::ALL {
            assert!(!setting.label().is_empty(), "{setting:?}");
        }
    }

    #[test]
    fn only_the_runtime_overrides_displace_a_file() {
        // A preset override changes how the runtime behaves; a "latest DLL"
        // override changes which runtime loads. Conflating them would have us
        // warning that an install will not take effect when it will.
        assert_eq!(
            Setting::SuperResolutionLatestDll.displaces_runtime(),
            Some(Feature::SuperResolution)
        );
        assert_eq!(
            Setting::NeuralRenderingLatestDll.displaces_runtime(),
            Some(Feature::NeuralRendering)
        );
        assert_eq!(Setting::SuperResolutionPreset.displaces_runtime(), None);
        assert_eq!(Setting::FrameGenerationPreset.displaces_runtime(), None);
    }

    fn value(setting: Setting, raw: u32) -> Value {
        Value {
            setting,
            value: raw,
            chosen_by_user: true,
        }
    }

    #[test]
    fn zero_means_off_for_every_override() {
        for setting in Setting::ALL {
            assert!(!value(setting, 0).overrides_our_install(), "{setting:?}");
        }
    }

    #[test]
    fn an_enabled_override_is_reported_as_a_warning_naming_the_features() {
        let profile = Profile {
            name: Some("Cyberpunk 2077".to_owned()),
            values: vec![
                value(Setting::SuperResolutionLatestDll, 1),
                value(Setting::FrameGenerationLatestDll, 1),
                // Off, so it must not appear.
                value(Setting::RayReconstructionLatestDll, 0),
                // A preset, which does not displace a file.
                value(Setting::NeuralRenderingPreset, 5),
            ],
        };

        assert_eq!(
            profile.overridden(),
            vec![Feature::SuperResolution, Feature::FrameGeneration]
        );
        let warning = profile.warning().expect("an override is set");
        assert!(warning.contains("Super Resolution"), "{warning}");
        assert!(warning.contains("Frame Generation"), "{warning}");
        assert!(!warning.contains("Ray Reconstruction"), "{warning}");
        assert!(!warning.contains("Neural Rendering"), "{warning}");
        // And it has to say what to do about it, not just that it happened.
        assert!(warning.contains("NVIDIA app"), "{warning}");
    }

    #[test]
    fn nothing_set_is_no_warning() {
        let quiet = Profile {
            name: None,
            values: Setting::ALL.iter().map(|s| value(*s, 0)).collect(),
        };
        assert!(quiet.overridden().is_empty());
        assert!(quiet.warning().is_none());
    }

    #[test]
    fn reading_the_driver_never_panics_and_never_half_answers() {
        // Runs against whatever this machine has. On a box with no NVIDIA
        // driver both return `None`; on one with a driver the values have to
        // be internally consistent. Either way it must not panic, because
        // this is the first place in the crate that calls into a foreign
        // library and a scan must survive it.
        for profile in [global(), for_executable("Cyberpunk2077.exe")]
            .into_iter()
            .flatten()
        {
            for value in &profile.values {
                // A value we report must belong to a setting we asked for.
                assert!(Setting::ALL.contains(&value.setting), "{value:?}");
            }
            // The warning is derived from the values, so the two cannot
            // disagree about whether anything is overridden.
            assert_eq!(
                profile.warning().is_some(),
                !profile.overridden().is_empty()
            );
        }
    }
}
