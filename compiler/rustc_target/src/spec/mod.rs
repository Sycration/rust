//! [Flexible target specification.](https://github.com/rust-lang/rfcs/pull/131)
//!
//! Rust targets a wide variety of usecases, and in the interest of flexibility,
//! allows new target triples to be defined in configuration files. Most users
//! will not need to care about these, but this is invaluable when porting Rust
//! to a new platform, and allows for an unprecedented level of control over how
//! the compiler works.
//!
//! # Using custom targets
//!
//! A target triple, as passed via `rustc --target=TRIPLE`, will first be
//! compared against the list of built-in targets. This is to ease distributing
//! rustc (no need for configuration files) and also to hold these built-in
//! targets as immutable and sacred. If `TRIPLE` is not one of the built-in
//! targets, rustc will check if a file named `TRIPLE` exists. If it does, it
//! will be loaded as the target configuration. If the file does not exist,
//! rustc will search each directory in the environment variable
//! `RUST_TARGET_PATH` for a file named `TRIPLE.json`. The first one found will
//! be loaded. If no file is found in any of those directories, a fatal error
//! will be given.
//!
//! Projects defining their own targets should use
//! `--target=path/to/my-awesome-platform.json` instead of adding to
//! `RUST_TARGET_PATH`.
//!
//! # Defining a new target
//!
//! Targets are defined using [JSON](http://json.org/). The `Target` struct in
//! this module defines the format the JSON file should take, though each
//! underscore in the field names should be replaced with a hyphen (`-`) in the
//! JSON file. Some fields are required in every target specification, such as
//! `llvm-target`, `target-endian`, `target-pointer-width`, `data-layout`,
//! `arch`, and `os`. In general, options passed to rustc with `-C` override
//! the target's settings, though `target-feature` and `link-args` will *add*
//! to the list specified by the target, rather than replace.

use crate::abi::Endian;
use crate::spec::abi::{lookup as lookup_abi, Abi};
use crate::spec::crt_objects::{CrtObjects, CrtObjectsFallback};
use rustc_serialize::json::{Json, ToJson};
use rustc_span::symbol::{sym, Symbol};
use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::{fmt, io};

use rustc_macros::HashStable_Generic;

pub mod abi;
pub mod crt_objects;

mod android_base;
mod apple_base;
mod apple_sdk_base;
mod arm_base;
mod avr_gnu_base;
mod dragonfly_base;
mod freebsd_base;
mod fuchsia_base;
mod haiku_base;
mod hermit_base;
mod hermit_kernel_base;
mod illumos_base;
mod l4re_base;
mod linux_base;
mod linux_gnu_base;
mod linux_kernel_base;
mod linux_musl_base;
mod linux_uclibc_base;
mod msvc_base;
mod netbsd_base;
mod openbsd_base;
mod redox_base;
mod riscv_base;
mod solaris_base;
mod thumb_base;
mod uefi_msvc_base;
mod vxworks_base;
mod wasm32_base;
mod windows_gnu_base;
mod windows_msvc_base;
mod windows_uwp_gnu_base;
mod windows_uwp_msvc_base;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LinkerFlavor {
    Em,
    Gcc,
    Ld,
    Msvc,
    Lld(LldFlavor),
    PtxLinker,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LldFlavor {
    Wasm,
    Ld64,
    Ld,
    Link,
}

impl LldFlavor {
    fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "darwin" => LldFlavor::Ld64,
            "gnu" => LldFlavor::Ld,
            "link" => LldFlavor::Link,
            "wasm" => LldFlavor::Wasm,
            _ => return None,
        })
    }
}

impl ToJson for LldFlavor {
    fn to_json(&self) -> Json {
        match *self {
            LldFlavor::Ld64 => "darwin",
            LldFlavor::Ld => "gnu",
            LldFlavor::Link => "link",
            LldFlavor::Wasm => "wasm",
        }
        .to_json()
    }
}

impl ToJson for LinkerFlavor {
    fn to_json(&self) -> Json {
        self.desc().to_json()
    }
}
macro_rules! flavor_mappings {
    ($((($($flavor:tt)*), $string:expr),)*) => (
        impl LinkerFlavor {
            pub const fn one_of() -> &'static str {
                concat!("one of: ", $($string, " ",)*)
            }

            pub fn from_str(s: &str) -> Option<Self> {
                Some(match s {
                    $($string => $($flavor)*,)*
                    _ => return None,
                })
            }

            pub fn desc(&self) -> &str {
                match *self {
                    $($($flavor)* => $string,)*
                }
            }
        }
    )
}

flavor_mappings! {
    ((LinkerFlavor::Em), "em"),
    ((LinkerFlavor::Gcc), "gcc"),
    ((LinkerFlavor::Ld), "ld"),
    ((LinkerFlavor::Msvc), "msvc"),
    ((LinkerFlavor::PtxLinker), "ptx-linker"),
    ((LinkerFlavor::Lld(LldFlavor::Wasm)), "wasm-ld"),
    ((LinkerFlavor::Lld(LldFlavor::Ld64)), "ld64.lld"),
    ((LinkerFlavor::Lld(LldFlavor::Ld)), "ld.lld"),
    ((LinkerFlavor::Lld(LldFlavor::Link)), "lld-link"),
}

#[derive(Clone, Copy, Debug, PartialEq, Hash, Encodable, Decodable, HashStable_Generic)]
pub enum PanicStrategy {
    Unwind,
    Abort,
}

impl PanicStrategy {
    pub fn desc(&self) -> &str {
        match *self {
            PanicStrategy::Unwind => "unwind",
            PanicStrategy::Abort => "abort",
        }
    }

    pub fn desc_symbol(&self) -> Symbol {
        match *self {
            PanicStrategy::Unwind => sym::unwind,
            PanicStrategy::Abort => sym::abort,
        }
    }
}

impl ToJson for PanicStrategy {
    fn to_json(&self) -> Json {
        match *self {
            PanicStrategy::Abort => "abort".to_json(),
            PanicStrategy::Unwind => "unwind".to_json(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Hash, Encodable, Decodable)]
pub enum RelroLevel {
    Full,
    Partial,
    Off,
    None,
}

impl RelroLevel {
    pub fn desc(&self) -> &str {
        match *self {
            RelroLevel::Full => "full",
            RelroLevel::Partial => "partial",
            RelroLevel::Off => "off",
            RelroLevel::None => "none",
        }
    }
}

impl FromStr for RelroLevel {
    type Err = ();

    fn from_str(s: &str) -> Result<RelroLevel, ()> {
        match s {
            "full" => Ok(RelroLevel::Full),
            "partial" => Ok(RelroLevel::Partial),
            "off" => Ok(RelroLevel::Off),
            "none" => Ok(RelroLevel::None),
            _ => Err(()),
        }
    }
}

impl ToJson for RelroLevel {
    fn to_json(&self) -> Json {
        match *self {
            RelroLevel::Full => "full".to_json(),
            RelroLevel::Partial => "partial".to_json(),
            RelroLevel::Off => "off".to_json(),
            RelroLevel::None => "None".to_json(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Hash, Encodable, Decodable)]
pub enum MergeFunctions {
    Disabled,
    Trampolines,
    Aliases,
}

impl MergeFunctions {
    pub fn desc(&self) -> &str {
        match *self {
            MergeFunctions::Disabled => "disabled",
            MergeFunctions::Trampolines => "trampolines",
            MergeFunctions::Aliases => "aliases",
        }
    }
}

impl FromStr for MergeFunctions {
    type Err = ();

    fn from_str(s: &str) -> Result<MergeFunctions, ()> {
        match s {
            "disabled" => Ok(MergeFunctions::Disabled),
            "trampolines" => Ok(MergeFunctions::Trampolines),
            "aliases" => Ok(MergeFunctions::Aliases),
            _ => Err(()),
        }
    }
}

impl ToJson for MergeFunctions {
    fn to_json(&self) -> Json {
        match *self {
            MergeFunctions::Disabled => "disabled".to_json(),
            MergeFunctions::Trampolines => "trampolines".to_json(),
            MergeFunctions::Aliases => "aliases".to_json(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Hash, Debug)]
pub enum RelocModel {
    Static,
    Pic,
    DynamicNoPic,
    Ropi,
    Rwpi,
    RopiRwpi,
}

impl FromStr for RelocModel {
    type Err = ();

    fn from_str(s: &str) -> Result<RelocModel, ()> {
        Ok(match s {
            "static" => RelocModel::Static,
            "pic" => RelocModel::Pic,
            "dynamic-no-pic" => RelocModel::DynamicNoPic,
            "ropi" => RelocModel::Ropi,
            "rwpi" => RelocModel::Rwpi,
            "ropi-rwpi" => RelocModel::RopiRwpi,
            _ => return Err(()),
        })
    }
}

impl ToJson for RelocModel {
    fn to_json(&self) -> Json {
        match *self {
            RelocModel::Static => "static",
            RelocModel::Pic => "pic",
            RelocModel::DynamicNoPic => "dynamic-no-pic",
            RelocModel::Ropi => "ropi",
            RelocModel::Rwpi => "rwpi",
            RelocModel::RopiRwpi => "ropi-rwpi",
        }
        .to_json()
    }
}

#[derive(Clone, Copy, PartialEq, Hash, Debug)]
pub enum CodeModel {
    Tiny,
    Small,
    Kernel,
    Medium,
    Large,
}

impl FromStr for CodeModel {
    type Err = ();

    fn from_str(s: &str) -> Result<CodeModel, ()> {
        Ok(match s {
            "tiny" => CodeModel::Tiny,
            "small" => CodeModel::Small,
            "kernel" => CodeModel::Kernel,
            "medium" => CodeModel::Medium,
            "large" => CodeModel::Large,
            _ => return Err(()),
        })
    }
}

impl ToJson for CodeModel {
    fn to_json(&self) -> Json {
        match *self {
            CodeModel::Tiny => "tiny",
            CodeModel::Small => "small",
            CodeModel::Kernel => "kernel",
            CodeModel::Medium => "medium",
            CodeModel::Large => "large",
        }
        .to_json()
    }
}

#[derive(Clone, Copy, PartialEq, Hash, Debug)]
pub enum TlsModel {
    GeneralDynamic,
    LocalDynamic,
    InitialExec,
    LocalExec,
}

impl FromStr for TlsModel {
    type Err = ();

    fn from_str(s: &str) -> Result<TlsModel, ()> {
        Ok(match s {
            // Note the difference "general" vs "global" difference. The model name is "general",
            // but the user-facing option name is "global" for consistency with other compilers.
            "global-dynamic" => TlsModel::GeneralDynamic,
            "local-dynamic" => TlsModel::LocalDynamic,
            "initial-exec" => TlsModel::InitialExec,
            "local-exec" => TlsModel::LocalExec,
            _ => return Err(()),
        })
    }
}

impl ToJson for TlsModel {
    fn to_json(&self) -> Json {
        match *self {
            TlsModel::GeneralDynamic => "global-dynamic",
            TlsModel::LocalDynamic => "local-dynamic",
            TlsModel::InitialExec => "initial-exec",
            TlsModel::LocalExec => "local-exec",
        }
        .to_json()
    }
}

/// Everything is flattened to a single enum to make the json encoding/decoding less annoying.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum LinkOutputKind {
    /// Dynamically linked non position-independent executable.
    DynamicNoPicExe,
    /// Dynamically linked position-independent executable.
    DynamicPicExe,
    /// Statically linked non position-independent executable.
    StaticNoPicExe,
    /// Statically linked position-independent executable.
    StaticPicExe,
    /// Regular dynamic library ("dynamically linked").
    DynamicDylib,
    /// Dynamic library with bundled libc ("statically linked").
    StaticDylib,
    /// WASI module with a lifetime past the _initialize entry point
    WasiReactorExe,
}

impl LinkOutputKind {
    fn as_str(&self) -> &'static str {
        match self {
            LinkOutputKind::DynamicNoPicExe => "dynamic-nopic-exe",
            LinkOutputKind::DynamicPicExe => "dynamic-pic-exe",
            LinkOutputKind::StaticNoPicExe => "static-nopic-exe",
            LinkOutputKind::StaticPicExe => "static-pic-exe",
            LinkOutputKind::DynamicDylib => "dynamic-dylib",
            LinkOutputKind::StaticDylib => "static-dylib",
            LinkOutputKind::WasiReactorExe => "wasi-reactor-exe",
        }
    }

    pub(super) fn from_str(s: &str) -> Option<LinkOutputKind> {
        Some(match s {
            "dynamic-nopic-exe" => LinkOutputKind::DynamicNoPicExe,
            "dynamic-pic-exe" => LinkOutputKind::DynamicPicExe,
            "static-nopic-exe" => LinkOutputKind::StaticNoPicExe,
            "static-pic-exe" => LinkOutputKind::StaticPicExe,
            "dynamic-dylib" => LinkOutputKind::DynamicDylib,
            "static-dylib" => LinkOutputKind::StaticDylib,
            "wasi-reactor-exe" => LinkOutputKind::WasiReactorExe,
            _ => return None,
        })
    }
}

impl fmt::Display for LinkOutputKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub type LinkArgs = BTreeMap<LinkerFlavor, Vec<String>>;

#[derive(Clone, Copy, Hash, Debug, PartialEq, Eq)]
pub enum SplitDebuginfo {
    /// Split debug-information is disabled, meaning that on supported platforms
    /// you can find all debug information in the executable itself. This is
    /// only supported for ELF effectively.
    ///
    /// * Windows - not supported
    /// * macOS - don't run `dsymutil`
    /// * ELF - `.dwarf_*` sections
    Off,

    /// Split debug-information can be found in a "packed" location separate
    /// from the final artifact. This is supported on all platforms.
    ///
    /// * Windows - `*.pdb`
    /// * macOS - `*.dSYM` (run `dsymutil`)
    /// * ELF - `*.dwp` (run `rust-llvm-dwp`)
    Packed,

    /// Split debug-information can be found in individual object files on the
    /// filesystem. The main executable may point to the object files.
    ///
    /// * Windows - not supported
    /// * macOS - supported, scattered object files
    /// * ELF - supported, scattered `*.dwo` files
    Unpacked,
}

impl SplitDebuginfo {
    fn as_str(&self) -> &'static str {
        match self {
            SplitDebuginfo::Off => "off",
            SplitDebuginfo::Packed => "packed",
            SplitDebuginfo::Unpacked => "unpacked",
        }
    }
}

impl FromStr for SplitDebuginfo {
    type Err = ();

    fn from_str(s: &str) -> Result<SplitDebuginfo, ()> {
        Ok(match s {
            "off" => SplitDebuginfo::Off,
            "unpacked" => SplitDebuginfo::Unpacked,
            "packed" => SplitDebuginfo::Packed,
            _ => return Err(()),
        })
    }
}

impl ToJson for SplitDebuginfo {
    fn to_json(&self) -> Json {
        self.as_str().to_json()
    }
}

impl fmt::Display for SplitDebuginfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

macro_rules! supported_targets {
    ( $(($( $triple:literal, )+ $module:ident ),)+ ) => {
        $(mod $module;)+

        /// List of supported targets
        pub const TARGETS: &[&str] = &[$($($triple),+),+];

        fn load_builtin(target: &str) -> Option<Target> {
            let mut t = match target {
                $( $($triple)|+ => $module::target(), )+
                _ => return None,
            };
            t.is_builtin = true;
            debug!("got builtin target: {:?}", t);
            Some(t)
        }

        #[cfg(test)]
        mod tests {
            mod tests_impl;

            // Cannot put this into a separate file without duplication, make an exception.
            $(
                #[test] // `#[test]`
                fn $module() {
                    tests_impl::test_target(super::$module::target());
                }
            )+
        }
    };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StackProbeType {
    /// Don't emit any stack probes.
    None,
    /// It is harmless to use this option even on targets that do not have backend support for
    /// stack probes as the failure mode is the same as if no stack-probe option was specified in
    /// the first place.
    Inline,
    /// Call `__rust_probestack` whenever stack needs to be probed.
    Call,
    /// Use inline option for LLVM versions later than specified in `min_llvm_version_for_inline`
    /// and call `__rust_probestack` otherwise.
    InlineOrCall { min_llvm_version_for_inline: (u32, u32, u32) },
}

impl StackProbeType {
    fn from_json(json: &Json) -> Result<Self, String> {
        let object = json.as_object().ok_or_else(|| "expected a JSON object")?;
        let kind = object
            .get("kind")
            .and_then(|o| o.as_string())
            .ok_or_else(|| "expected `kind` to be a string")?;
        match kind {
            "none" => Ok(StackProbeType::None),
            "inline" => Ok(StackProbeType::Inline),
            "call" => Ok(StackProbeType::Call),
            "inline-or-call" => {
                let min_version = object
                    .get("min-llvm-version-for-inline")
                    .and_then(|o| o.as_array())
                    .ok_or_else(|| "expected `min-llvm-version-for-inline` to be an array")?;
                let mut iter = min_version.into_iter().map(|v| {
                    let int = v.as_u64().ok_or_else(
                        || "expected `min-llvm-version-for-inline` values to be integers",
                    )?;
                    u32::try_from(int)
                        .map_err(|_| "`min-llvm-version-for-inline` values don't convert to u32")
                });
                let min_llvm_version_for_inline = (
                    iter.next().unwrap_or(Ok(11))?,
                    iter.next().unwrap_or(Ok(0))?,
                    iter.next().unwrap_or(Ok(0))?,
                );
                Ok(StackProbeType::InlineOrCall { min_llvm_version_for_inline })
            }
            _ => Err(String::from(
                "`kind` expected to be one of `none`, `inline`, `call` or `inline-or-call`",
            )),
        }
    }
}

impl ToJson for StackProbeType {
    fn to_json(&self) -> Json {
        Json::Object(match self {
            StackProbeType::None => {
                vec![(String::from("kind"), "none".to_json())].into_iter().collect()
            }
            StackProbeType::Inline => {
                vec![(String::from("kind"), "inline".to_json())].into_iter().collect()
            }
            StackProbeType::Call => {
                vec![(String::from("kind"), "call".to_json())].into_iter().collect()
            }
            StackProbeType::InlineOrCall { min_llvm_version_for_inline } => vec![
                (String::from("kind"), "inline-or-call".to_json()),
                (
                    String::from("min-llvm-version-for-inline"),
                    min_llvm_version_for_inline.to_json(),
                ),
            ]
            .into_iter()
            .collect(),
        })
    }
}

supported_targets! {
    ("x86_64-unknown-linux-gnu", x86_64_unknown_linux_gnu),
    ("x86_64-unknown-linux-gnux32", x86_64_unknown_linux_gnux32),
    ("i686-unknown-linux-gnu", i686_unknown_linux_gnu),
    ("i586-unknown-linux-gnu", i586_unknown_linux_gnu),
    ("mips-unknown-linux-gnu", mips_unknown_linux_gnu),
    ("mips64-unknown-linux-gnuabi64", mips64_unknown_linux_gnuabi64),
    ("mips64el-unknown-linux-gnuabi64", mips64el_unknown_linux_gnuabi64),
    ("mipsisa32r6-unknown-linux-gnu", mipsisa32r6_unknown_linux_gnu),
    ("mipsisa32r6el-unknown-linux-gnu", mipsisa32r6el_unknown_linux_gnu),
    ("mipsisa64r6-unknown-linux-gnuabi64", mipsisa64r6_unknown_linux_gnuabi64),
    ("mipsisa64r6el-unknown-linux-gnuabi64", mipsisa64r6el_unknown_linux_gnuabi64),
    ("mipsel-unknown-linux-gnu", mipsel_unknown_linux_gnu),
    ("powerpc-unknown-linux-gnu", powerpc_unknown_linux_gnu),
    ("powerpc-unknown-linux-gnuspe", powerpc_unknown_linux_gnuspe),
    ("powerpc-unknown-linux-musl", powerpc_unknown_linux_musl),
    ("powerpc64-unknown-linux-gnu", powerpc64_unknown_linux_gnu),
    ("powerpc64-unknown-linux-musl", powerpc64_unknown_linux_musl),
    ("powerpc64le-unknown-linux-gnu", powerpc64le_unknown_linux_gnu),
    ("powerpc64le-unknown-linux-musl", powerpc64le_unknown_linux_musl),
    ("s390x-unknown-linux-gnu", s390x_unknown_linux_gnu),
    ("s390x-unknown-linux-musl", s390x_unknown_linux_musl),
    ("sparc-unknown-linux-gnu", sparc_unknown_linux_gnu),
    ("sparc64-unknown-linux-gnu", sparc64_unknown_linux_gnu),
    ("arm-unknown-linux-gnueabi", arm_unknown_linux_gnueabi),
    ("arm-unknown-linux-gnueabihf", arm_unknown_linux_gnueabihf),
    ("arm-unknown-linux-musleabi", arm_unknown_linux_musleabi),
    ("arm-unknown-linux-musleabihf", arm_unknown_linux_musleabihf),
    ("armv4t-unknown-linux-gnueabi", armv4t_unknown_linux_gnueabi),
    ("armv5te-unknown-linux-gnueabi", armv5te_unknown_linux_gnueabi),
    ("armv5te-unknown-linux-musleabi", armv5te_unknown_linux_musleabi),
    ("armv5te-unknown-linux-uclibceabi", armv5te_unknown_linux_uclibceabi),
    ("armv7-unknown-linux-gnueabi", armv7_unknown_linux_gnueabi),
    ("armv7-unknown-linux-gnueabihf", armv7_unknown_linux_gnueabihf),
    ("thumbv7neon-unknown-linux-gnueabihf", thumbv7neon_unknown_linux_gnueabihf),
    ("thumbv7neon-unknown-linux-musleabihf", thumbv7neon_unknown_linux_musleabihf),
    ("armv7-unknown-linux-musleabi", armv7_unknown_linux_musleabi),
    ("armv7-unknown-linux-musleabihf", armv7_unknown_linux_musleabihf),
    ("aarch64-unknown-linux-gnu", aarch64_unknown_linux_gnu),
    ("aarch64-unknown-linux-musl", aarch64_unknown_linux_musl),
    ("x86_64-unknown-linux-musl", x86_64_unknown_linux_musl),
    ("i686-unknown-linux-musl", i686_unknown_linux_musl),
    ("i586-unknown-linux-musl", i586_unknown_linux_musl),
    ("i386_unknown_linux_gnu", i386_unknown_linux_gnu),
    ("i486_unknown_linux_gnu", i486_unknown_linux_gnu),
    ("mips-unknown-linux-musl", mips_unknown_linux_musl),
    ("mipsel-unknown-linux-musl", mipsel_unknown_linux_musl),
    ("mips64-unknown-linux-muslabi64", mips64_unknown_linux_muslabi64),
    ("mips64el-unknown-linux-muslabi64", mips64el_unknown_linux_muslabi64),
    ("hexagon-unknown-linux-musl", hexagon_unknown_linux_musl),

    ("mips-unknown-linux-uclibc", mips_unknown_linux_uclibc),
    ("mipsel-unknown-linux-uclibc", mipsel_unknown_linux_uclibc),

    ("i686-linux-android", i686_linux_android),
    ("x86_64-linux-android", x86_64_linux_android),
    ("arm-linux-androideabi", arm_linux_androideabi),
    ("armv7-linux-androideabi", armv7_linux_androideabi),
    ("thumbv7neon-linux-androideabi", thumbv7neon_linux_androideabi),
    ("aarch64-linux-android", aarch64_linux_android),

    ("x86_64-unknown-none-linuxkernel", x86_64_unknown_none_linuxkernel),

    ("aarch64-unknown-freebsd", aarch64_unknown_freebsd),
    ("armv6-unknown-freebsd", armv6_unknown_freebsd),
    ("armv7-unknown-freebsd", armv7_unknown_freebsd),
    ("i686-unknown-freebsd", i686_unknown_freebsd),
    ("powerpc64-unknown-freebsd", powerpc64_unknown_freebsd),
    ("x86_64-unknown-freebsd", x86_64_unknown_freebsd),

    ("x86_64-unknown-dragonfly", x86_64_unknown_dragonfly),

    ("aarch64-unknown-openbsd", aarch64_unknown_openbsd),
    ("i686-unknown-openbsd", i686_unknown_openbsd),
    ("sparc64-unknown-openbsd", sparc64_unknown_openbsd),
    ("x86_64-unknown-openbsd", x86_64_unknown_openbsd),
    ("powerpc-unknown-openbsd", powerpc_unknown_openbsd),

    ("aarch64-unknown-netbsd", aarch64_unknown_netbsd),
    ("armv6-unknown-netbsd-eabihf", armv6_unknown_netbsd_eabihf),
    ("armv7-unknown-netbsd-eabihf", armv7_unknown_netbsd_eabihf),
    ("i686-unknown-netbsd", i686_unknown_netbsd),
    ("powerpc-unknown-netbsd", powerpc_unknown_netbsd),
    ("sparc64-unknown-netbsd", sparc64_unknown_netbsd),
    ("x86_64-unknown-netbsd", x86_64_unknown_netbsd),

    ("i686-unknown-haiku", i686_unknown_haiku),
    ("x86_64-unknown-haiku", x86_64_unknown_haiku),

    ("aarch64-apple-darwin", aarch64_apple_darwin),
    ("x86_64-apple-darwin", x86_64_apple_darwin),
    ("i686-apple-darwin", i686_apple_darwin),

    ("aarch64-fuchsia", aarch64_fuchsia),
    ("x86_64-fuchsia", x86_64_fuchsia),

    ("avr-unknown-gnu-atmega328", avr_unknown_gnu_atmega328),

    ("x86_64-unknown-l4re-uclibc", x86_64_unknown_l4re_uclibc),

    ("aarch64-unknown-redox", aarch64_unknown_redox),
    ("x86_64-unknown-redox", x86_64_unknown_redox),

    ("i386-apple-ios", i386_apple_ios),
    ("x86_64-apple-ios", x86_64_apple_ios),
    ("aarch64-apple-ios", aarch64_apple_ios),
    ("armv7-apple-ios", armv7_apple_ios),
    ("armv7s-apple-ios", armv7s_apple_ios),
    ("x86_64-apple-ios-macabi", x86_64_apple_ios_macabi),
    ("aarch64-apple-ios-macabi", aarch64_apple_ios_macabi),
    ("aarch64-apple-ios-sim", aarch64_apple_ios_sim),
    ("aarch64-apple-tvos", aarch64_apple_tvos),
    ("x86_64-apple-tvos", x86_64_apple_tvos),

    ("armebv7r-none-eabi", armebv7r_none_eabi),
    ("armebv7r-none-eabihf", armebv7r_none_eabihf),
    ("armv7r-none-eabi", armv7r_none_eabi),
    ("armv7r-none-eabihf", armv7r_none_eabihf),

    ("x86_64-pc-solaris", x86_64_pc_solaris),
    ("x86_64-sun-solaris", x86_64_sun_solaris),
    ("sparcv9-sun-solaris", sparcv9_sun_solaris),

    ("x86_64-unknown-illumos", x86_64_unknown_illumos),

    ("x86_64-pc-windows-gnu", x86_64_pc_windows_gnu),
    ("i686-pc-windows-gnu", i686_pc_windows_gnu),
    ("i686-uwp-windows-gnu", i686_uwp_windows_gnu),
    ("x86_64-uwp-windows-gnu", x86_64_uwp_windows_gnu),

    ("aarch64-pc-windows-msvc", aarch64_pc_windows_msvc),
    ("aarch64-uwp-windows-msvc", aarch64_uwp_windows_msvc),
    ("x86_64-pc-windows-msvc", x86_64_pc_windows_msvc),
    ("x86_64-uwp-windows-msvc", x86_64_uwp_windows_msvc),
    ("i686-pc-windows-msvc", i686_pc_windows_msvc),
    ("i686-uwp-windows-msvc", i686_uwp_windows_msvc),
    ("i586-pc-windows-msvc", i586_pc_windows_msvc),
    ("thumbv7a-pc-windows-msvc", thumbv7a_pc_windows_msvc),
    ("thumbv7a-uwp-windows-msvc", thumbv7a_uwp_windows_msvc),

    ("asmjs-unknown-emscripten", asmjs_unknown_emscripten),
    ("wasm32-unknown-emscripten", wasm32_unknown_emscripten),
    ("wasm32-unknown-unknown", wasm32_unknown_unknown),
    ("wasm32-wasi", wasm32_wasi),

    ("thumbv6m-none-eabi", thumbv6m_none_eabi),
    ("thumbv7m-none-eabi", thumbv7m_none_eabi),
    ("thumbv7em-none-eabi", thumbv7em_none_eabi),
    ("thumbv7em-none-eabihf", thumbv7em_none_eabihf),
    ("thumbv8m.base-none-eabi", thumbv8m_base_none_eabi),
    ("thumbv8m.main-none-eabi", thumbv8m_main_none_eabi),
    ("thumbv8m.main-none-eabihf", thumbv8m_main_none_eabihf),

    ("armv7a-none-eabi", armv7a_none_eabi),
    ("armv7a-none-eabihf", armv7a_none_eabihf),

    ("msp430-none-elf", msp430_none_elf),

    ("aarch64-unknown-hermit", aarch64_unknown_hermit),
    ("x86_64-unknown-hermit", x86_64_unknown_hermit),

    ("x86_64-unknown-none-hermitkernel", x86_64_unknown_none_hermitkernel),

    ("riscv32i-unknown-none-elf", riscv32i_unknown_none_elf),
    ("riscv32imc-unknown-none-elf", riscv32imc_unknown_none_elf),
    ("riscv32imac-unknown-none-elf", riscv32imac_unknown_none_elf),
    ("riscv32gc-unknown-linux-gnu", riscv32gc_unknown_linux_gnu),
    ("riscv32gc-unknown-linux-musl", riscv32gc_unknown_linux_musl),
    ("riscv64imac-unknown-none-elf", riscv64imac_unknown_none_elf),
    ("riscv64gc-unknown-none-elf", riscv64gc_unknown_none_elf),
    ("riscv64gc-unknown-linux-gnu", riscv64gc_unknown_linux_gnu),
    ("riscv64gc-unknown-linux-musl", riscv64gc_unknown_linux_musl),

    ("aarch64-unknown-none", aarch64_unknown_none),
    ("aarch64-unknown-none-softfloat", aarch64_unknown_none_softfloat),

    ("x86_64-fortanix-unknown-sgx", x86_64_fortanix_unknown_sgx),

    ("x86_64-unknown-uefi", x86_64_unknown_uefi),
    ("i686-unknown-uefi", i686_unknown_uefi),

    ("nvptx64-nvidia-cuda", nvptx64_nvidia_cuda),

    ("i686-wrs-vxworks", i686_wrs_vxworks),
    ("x86_64-wrs-vxworks", x86_64_wrs_vxworks),
    ("armv7-wrs-vxworks-eabihf", armv7_wrs_vxworks_eabihf),
    ("aarch64-wrs-vxworks", aarch64_wrs_vxworks),
    ("powerpc-wrs-vxworks", powerpc_wrs_vxworks),
    ("powerpc-wrs-vxworks-spe", powerpc_wrs_vxworks_spe),
    ("powerpc64-wrs-vxworks", powerpc64_wrs_vxworks),

    ("mipsel-sony-psp", mipsel_sony_psp),
    ("mipsel-unknown-none", mipsel_unknown_none),
    ("thumbv4t-none-eabi", thumbv4t_none_eabi),

    ("aarch64_be-unknown-linux-gnu", aarch64_be_unknown_linux_gnu),
    ("aarch64-unknown-linux-gnu_ilp32", aarch64_unknown_linux_gnu_ilp32),
    ("aarch64_be-unknown-linux-gnu_ilp32", aarch64_be_unknown_linux_gnu_ilp32),
}

/// Everything `rustc` knows about how to compile for a specific target.
///
/// Every field here must be specified, and has no default value.
#[derive(PartialEq, Clone, Debug)]
pub struct Target {
    /// Target triple to pass to LLVM.
    pub llvm_target: String,
    /// Number of bits in a pointer. Influences the `target_pointer_width` `cfg` variable.
    pub pointer_width: u32,
    /// Architecture to use for ABI considerations. Valid options include: "x86",
    /// "x86_64", "arm", "aarch64", "mips", "powerpc", "powerpc64", and others.
    pub arch: String,
    /// [Data layout](http://llvm.org/docs/LangRef.html#data-layout) to pass to LLVM.
    pub data_layout: String,
    /// Optional settings with defaults.
    pub options: TargetOptions,
}

pub trait HasTargetSpec {
    fn target_spec(&self) -> &Target;
}

impl HasTargetSpec for Target {
    fn target_spec(&self) -> &Target {
        self
    }
}

/// Optional aspects of a target specification.
///
/// This has an implementation of `Default`, see each field for what the default is. In general,
/// these try to take "minimal defaults" that don't assume anything about the runtime they run in.
///
/// `TargetOptions` as a separate structure is mostly an implementation detail of `Target`
/// construction, all its fields logically belong to `Target` and available from `Target`
/// through `Deref` impls.
#[derive(PartialEq, Clone, Debug)]
pub struct TargetOptions {
    /// Whether the target is built-in or loaded from a custom target specification.
    pub is_builtin: bool,

    /// Used as the `target_endian` `cfg` variable. Defaults to little endian.
    pub endian: Endian,
    /// Width of c_int type. Defaults to "32".
    pub c_int_width: String,
    /// OS name to use for conditional compilation (`target_os`). Defaults to "none".
    /// "none" implies a bare metal target without `std` library.
    /// A couple of targets having `std` also use "unknown" as an `os` value,
    /// but they are exceptions.
    pub os: String,
    /// Environment name to use for conditional compilation (`target_env`). Defaults to "".
    pub env: String,
    /// Vendor name to use for conditional compilation (`target_vendor`). Defaults to "unknown".
    pub vendor: String,
    /// Default linker flavor used if `-C linker-flavor` or `-C linker` are not passed
    /// on the command line. Defaults to `LinkerFlavor::Gcc`.
    pub linker_flavor: LinkerFlavor,

    /// Linker to invoke
    pub linker: Option<String>,

    /// LLD flavor used if `lld` (or `rust-lld`) is specified as a linker
    /// without clarifying its flavor in any way.
    pub lld_flavor: LldFlavor,

    /// Linker arguments that are passed *before* any user-defined libraries.
    pub pre_link_args: LinkArgs,
    /// Objects to link before and after all other object code.
    pub pre_link_objects: CrtObjects,
    pub post_link_objects: CrtObjects,
    /// Same as `(pre|post)_link_objects`, but when we fail to pull the objects with help of the
    /// target's native gcc and fall back to the "self-contained" mode and pull them manually.
    /// See `crt_objects.rs` for some more detailed documentation.
    pub pre_link_objects_fallback: CrtObjects,
    pub post_link_objects_fallback: CrtObjects,
    /// Which logic to use to determine whether to fall back to the "self-contained" mode or not.
    pub crt_objects_fallback: Option<CrtObjectsFallback>,

    /// Linker arguments that are unconditionally passed after any
    /// user-defined but before post-link objects. Standard platform
    /// libraries that should be always be linked to, usually go here.
    pub late_link_args: LinkArgs,
    /// Linker arguments used in addition to `late_link_args` if at least one
    /// Rust dependency is dynamically linked.
    pub late_link_args_dynamic: LinkArgs,
    /// Linker arguments used in addition to `late_link_args` if aall Rust
    /// dependencies are statically linked.
    pub late_link_args_static: LinkArgs,
    /// Linker arguments that are unconditionally passed *after* any
    /// user-defined libraries.
    pub post_link_args: LinkArgs,
    /// Optional link script applied to `dylib` and `executable` crate types.
    /// This is a string containing the script, not a path. Can only be applied
    /// to linkers where `linker_is_gnu` is true.
    pub link_script: Option<String>,

    /// Environment variables to be set for the linker invocation.
    pub link_env: Vec<(String, String)>,
    /// Environment variables to be removed for the linker invocation.
    pub link_env_remove: Vec<String>,

    /// Extra arguments to pass to the external assembler (when used)
    pub asm_args: Vec<String>,

    /// Default CPU to pass to LLVM. Corresponds to `llc -mcpu=$cpu`. Defaults
    /// to "generic".
    pub cpu: String,
    /// Default target features to pass to LLVM. These features will *always* be
    /// passed, and cannot be disabled even via `-C`. Corresponds to `llc
    /// -mattr=$features`.
    pub features: String,
    /// Whether dynamic linking is available on this target. Defaults to false.
    pub dynamic_linking: bool,
    /// If dynamic linking is available, whether only cdylibs are supported.
    pub only_cdylib: bool,
    /// Whether executables are available on this target. iOS, for example, only allows static
    /// libraries. Defaults to false.
    pub executables: bool,
    /// Relocation model to use in object file. Corresponds to `llc
    /// -relocation-model=$relocation_model`. Defaults to `Pic`.
    pub relocation_model: RelocModel,
    /// Code model to use. Corresponds to `llc -code-model=$code_model`.
    /// Defaults to `None` which means "inherited from the base LLVM target".
    pub code_model: Option<CodeModel>,
    /// TLS model to use. Options are "global-dynamic" (default), "local-dynamic", "initial-exec"
    /// and "local-exec". This is similar to the -ftls-model option in GCC/Clang.
    pub tls_model: TlsModel,
    /// Do not emit code that uses the "red zone", if the ABI has one. Defaults to false.
    pub disable_redzone: bool,
    /// Eliminate frame pointers from stack frames if possible. Defaults to true.
    pub eliminate_frame_pointer: bool,
    /// Emit each function in its own section. Defaults to true.
    pub function_sections: bool,
    /// String to prepend to the name of every dynamic library. Defaults to "lib".
    pub dll_prefix: String,
    /// String to append to the name of every dynamic library. Defaults to ".so".
    pub dll_suffix: String,
    /// String to append to the name of every executable.
    pub exe_suffix: String,
    /// String to prepend to the name of every static library. Defaults to "lib".
    pub staticlib_prefix: String,
    /// String to append to the name of every static library. Defaults to ".a".
    pub staticlib_suffix: String,
    /// OS family to use for conditional compilation. Valid options: "unix", "windows".
    pub os_family: Option<String>,
    /// Whether the target toolchain's ABI supports returning small structs as an integer.
    pub abi_return_struct_as_int: bool,
    /// Whether the target toolchain is like macOS's. Only useful for compiling against iOS/macOS,
    /// in particular running dsymutil and some other stuff like `-dead_strip`. Defaults to false.
    pub is_like_osx: bool,
    /// Whether the target toolchain is like Solaris's.
    /// Only useful for compiling against Illumos/Solaris,
    /// as they have a different set of linker flags. Defaults to false.
    pub is_like_solaris: bool,
    /// Whether the target is like Windows.
    /// This is a combination of several more specific properties represented as a single flag:
    ///   - The target uses a Windows ABI,
    ///   - uses PE/COFF as a format for object code,
    ///   - uses Windows-style dllexport/dllimport for shared libraries,
    ///   - uses import libraries and .def files for symbol exports,
    ///   - executables support setting a subsystem.
    pub is_like_windows: bool,
    /// Whether the target is like MSVC.
    /// This is a combination of several more specific properties represented as a single flag:
    ///   - The target has all the properties from `is_like_windows`
    ///     (for in-tree targets "is_like_msvc ⇒ is_like_windows" is ensured by a unit test),
    ///   - has some MSVC-specific Windows ABI properties,
    ///   - uses a link.exe-like linker,
    ///   - uses CodeView/PDB for debuginfo and natvis for its visualization,
    ///   - uses SEH-based unwinding,
    ///   - supports control flow guard mechanism.
    pub is_like_msvc: bool,
    /// Whether the target toolchain is like Emscripten's. Only useful for compiling with
    /// Emscripten toolchain.
    /// Defaults to false.
    pub is_like_emscripten: bool,
    /// Whether the target toolchain is like Fuchsia's.
    pub is_like_fuchsia: bool,
    /// Version of DWARF to use if not using the default.
    /// Useful because some platforms (osx, bsd) only want up to DWARF2.
    pub dwarf_version: Option<u32>,
    /// Whether the linker support GNU-like arguments such as -O. Defaults to false.
    pub linker_is_gnu: bool,
    /// The MinGW toolchain has a known issue that prevents it from correctly
    /// handling COFF object files with more than 2<sup>15</sup> sections. Since each weak
    /// symbol needs its own COMDAT section, weak linkage implies a large
    /// number sections that easily exceeds the given limit for larger
    /// codebases. Consequently we want a way to disallow weak linkage on some
    /// platforms.
    pub allows_weak_linkage: bool,
    /// Whether the linker support rpaths or not. Defaults to false.
    pub has_rpath: bool,
    /// Whether to disable linking to the default libraries, typically corresponds
    /// to `-nodefaultlibs`. Defaults to true.
    pub no_default_libraries: bool,
    /// Dynamically linked executables can be compiled as position independent
    /// if the default relocation model of position independent code is not
    /// changed. This is a requirement to take advantage of ASLR, as otherwise
    /// the functions in the executable are not randomized and can be used
    /// during an exploit of a vulnerability in any code.
    pub position_independent_executables: bool,
    /// Executables that are both statically linked and position-independent are supported.
    pub static_position_independent_executables: bool,
    /// Determines if the target always requires using the PLT for indirect
    /// library calls or not. This controls the default value of the `-Z plt` flag.
    pub needs_plt: bool,
    /// Either partial, full, or off. Full RELRO makes the dynamic linker
    /// resolve all symbols at startup and marks the GOT read-only before
    /// starting the program, preventing overwriting the GOT.
    pub relro_level: RelroLevel,
    /// Format that archives should be emitted in. This affects whether we use
    /// LLVM to assemble an archive or fall back to the system linker, and
    /// currently only "gnu" is used to fall into LLVM. Unknown strings cause
    /// the system linker to be used.
    pub archive_format: String,
    /// Is asm!() allowed? Defaults to true.
    pub allow_asm: bool,
    /// Whether the runtime startup code requires the `main` function be passed
    /// `argc` and `argv` values.
    pub main_needs_argc_argv: bool,

    /// Flag indicating whether ELF TLS (e.g., #[thread_local]) is available for
    /// this target.
    pub has_elf_tls: bool,
    // This is mainly for easy compatibility with emscripten.
    // If we give emcc .o files that are actually .bc files it
    // will 'just work'.
    pub obj_is_bitcode: bool,
    /// Whether the target requires that emitted object code includes bitcode.
    pub forces_embed_bitcode: bool,
    /// Content of the LLVM cmdline section associated with embedded bitcode.
    pub bitcode_llvm_cmdline: String,

    /// Don't use this field; instead use the `.min_atomic_width()` method.
    pub min_atomic_width: Option<u64>,

    /// Don't use this field; instead use the `.max_atomic_width()` method.
    pub max_atomic_width: Option<u64>,

    /// Whether the target supports atomic CAS operations natively
    pub atomic_cas: bool,

    /// Panic strategy: "unwind" or "abort"
    pub panic_strategy: PanicStrategy,

    /// A list of ABIs unsupported by the current target. Note that generic ABIs
    /// are considered to be supported on all platforms and cannot be marked
    /// unsupported.
    pub unsupported_abis: Vec<Abi>,

    /// Whether or not linking dylibs to a static CRT is allowed.
    pub crt_static_allows_dylibs: bool,
    /// Whether or not the CRT is statically linked by default.
    pub crt_static_default: bool,
    /// Whether or not crt-static is respected by the compiler (or is a no-op).
    pub crt_static_respected: bool,

    /// The implementation of stack probes to use.
    pub stack_probes: StackProbeType,

    /// The minimum alignment for global symbols.
    pub min_global_align: Option<u64>,

    /// Default number of codegen units to use in debug mode
    pub default_codegen_units: Option<u64>,

    /// Whether to generate trap instructions in places where optimization would
    /// otherwise produce control flow that falls through into unrelated memory.
    pub trap_unreachable: bool,

    /// This target requires everything to be compiled with LTO to emit a final
    /// executable, aka there is no native linker for this target.
    pub requires_lto: bool,

    /// This target has no support for threads.
    pub singlethread: bool,

    /// Whether library functions call lowering/optimization is disabled in LLVM
    /// for this target unconditionally.
    pub no_builtins: bool,

    /// The default visibility for symbols in this target should be "hidden"
    /// rather than "default"
    pub default_hidden_visibility: bool,

    /// Whether a .debug_gdb_scripts section will be added to the output object file
    pub emit_debug_gdb_scripts: bool,

    /// Whether or not to unconditionally `uwtable` attributes on functions,
    /// typically because the platform needs to unwind for things like stack
    /// unwinders.
    pub requires_uwtable: bool,

    /// Whether or not to emit `uwtable` attributes on functions if `-C force-unwind-tables`
    /// is not specified and `uwtable` is not required on this target.
    pub default_uwtable: bool,

    /// Whether or not SIMD types are passed by reference in the Rust ABI,
    /// typically required if a target can be compiled with a mixed set of
    /// target features. This is `true` by default, and `false` for targets like
    /// wasm32 where the whole program either has simd or not.
    pub simd_types_indirect: bool,

    /// Pass a list of symbol which should be exported in the dylib to the linker.
    pub limit_rdylib_exports: bool,

    /// If set, have the linker export exactly these symbols, instead of using
    /// the usual logic to figure this out from the crate itself.
    pub override_export_symbols: Option<Vec<String>>,

    /// Determines how or whether the MergeFunctions LLVM pass should run for
    /// this target. Either "disabled", "trampolines", or "aliases".
    /// The MergeFunctions pass is generally useful, but some targets may need
    /// to opt out. The default is "aliases".
    ///
    /// Workaround for: <https://github.com/rust-lang/rust/issues/57356>
    pub merge_functions: MergeFunctions,

    /// Use platform dependent mcount function
    pub mcount: String,

    /// LLVM ABI name, corresponds to the '-mabi' parameter available in multilib C compilers
    pub llvm_abiname: String,

    /// Whether or not RelaxElfRelocation flag will be passed to the linker
    pub relax_elf_relocations: bool,

    /// Additional arguments to pass to LLVM, similar to the `-C llvm-args` codegen option.
    pub llvm_args: Vec<String>,

    /// Whether to use legacy .ctors initialization hooks rather than .init_array. Defaults
    /// to false (uses .init_array).
    pub use_ctors_section: bool,

    /// Whether the linker is instructed to add a `GNU_EH_FRAME` ELF header
    /// used to locate unwinding information is passed
    /// (only has effect if the linker is `ld`-like).
    pub eh_frame_header: bool,

    /// Is true if the target is an ARM architecture using thumb v1 which allows for
    /// thumb and arm interworking.
    pub has_thumb_interworking: bool,

    /// How to handle split debug information, if at all. Specifying `None` has
    /// target-specific meaning.
    pub split_debuginfo: SplitDebuginfo,
}

impl Default for TargetOptions {
    /// Creates a set of "sane defaults" for any target. This is still
    /// incomplete, and if used for compilation, will certainly not work.
    fn default() -> TargetOptions {
        TargetOptions {
            is_builtin: false,
            endian: Endian::Little,
            c_int_width: "32".to_string(),
            os: "none".to_string(),
            env: String::new(),
            vendor: "unknown".to_string(),
            linker_flavor: LinkerFlavor::Gcc,
            linker: option_env!("CFG_DEFAULT_LINKER").map(|s| s.to_string()),
            lld_flavor: LldFlavor::Ld,
            pre_link_args: LinkArgs::new(),
            post_link_args: LinkArgs::new(),
            link_script: None,
            asm_args: Vec::new(),
            cpu: "generic".to_string(),
            features: String::new(),
            dynamic_linking: false,
            only_cdylib: false,
            executables: false,
            relocation_model: RelocModel::Pic,
            code_model: None,
            tls_model: TlsModel::GeneralDynamic,
            disable_redzone: false,
            eliminate_frame_pointer: true,
            function_sections: true,
            dll_prefix: "lib".to_string(),
            dll_suffix: ".so".to_string(),
            exe_suffix: String::new(),
            staticlib_prefix: "lib".to_string(),
            staticlib_suffix: ".a".to_string(),
            os_family: None,
            abi_return_struct_as_int: false,
            is_like_osx: false,
            is_like_solaris: false,
            is_like_windows: false,
            is_like_emscripten: false,
            is_like_msvc: false,
            is_like_fuchsia: false,
            dwarf_version: None,
            linker_is_gnu: false,
            allows_weak_linkage: true,
            has_rpath: false,
            no_default_libraries: true,
            position_independent_executables: false,
            static_position_independent_executables: false,
            needs_plt: false,
            relro_level: RelroLevel::None,
            pre_link_objects: Default::default(),
            post_link_objects: Default::default(),
            pre_link_objects_fallback: Default::default(),
            post_link_objects_fallback: Default::default(),
            crt_objects_fallback: None,
            late_link_args: LinkArgs::new(),
            late_link_args_dynamic: LinkArgs::new(),
            late_link_args_static: LinkArgs::new(),
            link_env: Vec::new(),
            link_env_remove: Vec::new(),
            archive_format: "gnu".to_string(),
            main_needs_argc_argv: true,
            allow_asm: true,
            has_elf_tls: false,
            obj_is_bitcode: false,
            forces_embed_bitcode: false,
            bitcode_llvm_cmdline: String::new(),
            min_atomic_width: None,
            max_atomic_width: None,
            atomic_cas: true,
            panic_strategy: PanicStrategy::Unwind,
            unsupported_abis: vec![],
            crt_static_allows_dylibs: false,
            crt_static_default: false,
            crt_static_respected: false,
            stack_probes: StackProbeType::None,
            min_global_align: None,
            default_codegen_units: None,
            trap_unreachable: true,
            requires_lto: false,
            singlethread: false,
            no_builtins: false,
            default_hidden_visibility: false,
            emit_debug_gdb_scripts: true,
            requires_uwtable: false,
            default_uwtable: false,
            simd_types_indirect: true,
            limit_rdylib_exports: true,
            override_export_symbols: None,
            merge_functions: MergeFunctions::Aliases,
            mcount: "mcount".to_string(),
            llvm_abiname: "".to_string(),
            relax_elf_relocations: false,
            llvm_args: vec![],
            use_ctors_section: false,
            eh_frame_header: true,
            has_thumb_interworking: false,
            split_debuginfo: SplitDebuginfo::Off,
        }
    }
}

/// `TargetOptions` being a separate type is basically an implementation detail of `Target` that is
/// used for providing defaults. Perhaps there's a way to merge `TargetOptions` into `Target` so
/// this `Deref` implementation is no longer necessary.
impl Deref for Target {
    type Target = TargetOptions;

    fn deref(&self) -> &Self::Target {
        &self.options
    }
}
impl DerefMut for Target {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.options
    }
}

impl Target {
    /// Given a function ABI, turn it into the correct ABI for this target.
    pub fn adjust_abi(&self, abi: Abi) -> Abi {
        match abi {
            Abi::System { unwind } => {
                if self.is_like_windows && self.arch == "x86" {
                    Abi::Stdcall { unwind }
                } else {
                    Abi::C { unwind }
                }
            }
            // These ABI kinds are ignored on non-x86 Windows targets.
            // See https://docs.microsoft.com/en-us/cpp/cpp/argument-passing-and-naming-conventions
            // and the individual pages for __stdcall et al.
            Abi::Stdcall { unwind } | Abi::Thiscall { unwind } => {
                if self.is_like_windows && self.arch != "x86" { Abi::C { unwind } } else { abi }
            }
            Abi::Fastcall | Abi::Vectorcall => {
                if self.is_like_windows && self.arch != "x86" {
                    Abi::C { unwind: false }
                } else {
                    abi
                }
            }
            Abi::EfiApi => {
                if self.arch == "x86_64" {
                    Abi::Win64
                } else {
                    Abi::C { unwind: false }
                }
            }
            abi => abi,
        }
    }

    /// Minimum integer size in bits that this target can perform atomic
    /// operations on.
    pub fn min_atomic_width(&self) -> u64 {
        self.min_atomic_width.unwrap_or(8)
    }

    /// Maximum integer size in bits that this target can perform atomic
    /// operations on.
    pub fn max_atomic_width(&self) -> u64 {
        self.max_atomic_width.unwrap_or_else(|| self.pointer_width.into())
    }

    pub fn is_abi_supported(&self, abi: Abi) -> bool {
        abi.generic() || !self.unsupported_abis.contains(&abi)
    }

    /// Loads a target descriptor from a JSON object.
    pub fn from_json(obj: Json) -> Result<Target, String> {
        // While ugly, this code must remain this way to retain
        // compatibility with existing JSON fields and the internal
        // expected naming of the Target and TargetOptions structs.
        // To ensure compatibility is retained, the built-in targets
        // are round-tripped through this code to catch cases where
        // the JSON parser is not updated to match the structs.

        let get_req_field = |name: &str| {
            obj.find(name)
                .map(|s| s.as_string())
                .and_then(|os| os.map(|s| s.to_string()))
                .ok_or_else(|| format!("Field {} in target specification is required", name))
        };

        let mut base = Target {
            llvm_target: get_req_field("llvm-target")?,
            pointer_width: get_req_field("target-pointer-width")?
                .parse::<u32>()
                .map_err(|_| "target-pointer-width must be an integer".to_string())?,
            data_layout: get_req_field("data-layout")?,
            arch: get_req_field("arch")?,
            options: Default::default(),
        };

        macro_rules! key {
            ($key_name:ident) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                if let Some(s) = obj.find(&name).and_then(Json::as_string) {
                    base.$key_name = s.to_string();
                }
            } );
            ($key_name:ident = $json_name:expr) => ( {
                let name = $json_name;
                if let Some(s) = obj.find(&name).and_then(Json::as_string) {
                    base.$key_name = s.to_string();
                }
            } );
            ($key_name:ident, bool) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                if let Some(s) = obj.find(&name).and_then(Json::as_boolean) {
                    base.$key_name = s;
                }
            } );
            ($key_name:ident, Option<u32>) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                if let Some(s) = obj.find(&name).and_then(Json::as_u64) {
                    if s < 1 || s > 5 {
                        return Err("Not a valid DWARF version number".to_string());
                    }
                    base.$key_name = Some(s as u32);
                }
            } );
            ($key_name:ident, Option<u64>) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                if let Some(s) = obj.find(&name).and_then(Json::as_u64) {
                    base.$key_name = Some(s);
                }
            } );
            ($key_name:ident, MergeFunctions) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                obj.find(&name[..]).and_then(|o| o.as_string().and_then(|s| {
                    match s.parse::<MergeFunctions>() {
                        Ok(mergefunc) => base.$key_name = mergefunc,
                        _ => return Some(Err(format!("'{}' is not a valid value for \
                                                      merge-functions. Use 'disabled', \
                                                      'trampolines', or 'aliases'.",
                                                      s))),
                    }
                    Some(Ok(()))
                })).unwrap_or(Ok(()))
            } );
            ($key_name:ident, RelocModel) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                obj.find(&name[..]).and_then(|o| o.as_string().and_then(|s| {
                    match s.parse::<RelocModel>() {
                        Ok(relocation_model) => base.$key_name = relocation_model,
                        _ => return Some(Err(format!("'{}' is not a valid relocation model. \
                                                      Run `rustc --print relocation-models` to \
                                                      see the list of supported values.", s))),
                    }
                    Some(Ok(()))
                })).unwrap_or(Ok(()))
            } );
            ($key_name:ident, CodeModel) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                obj.find(&name[..]).and_then(|o| o.as_string().and_then(|s| {
                    match s.parse::<CodeModel>() {
                        Ok(code_model) => base.$key_name = Some(code_model),
                        _ => return Some(Err(format!("'{}' is not a valid code model. \
                                                      Run `rustc --print code-models` to \
                                                      see the list of supported values.", s))),
                    }
                    Some(Ok(()))
                })).unwrap_or(Ok(()))
            } );
            ($key_name:ident, TlsModel) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                obj.find(&name[..]).and_then(|o| o.as_string().and_then(|s| {
                    match s.parse::<TlsModel>() {
                        Ok(tls_model) => base.$key_name = tls_model,
                        _ => return Some(Err(format!("'{}' is not a valid TLS model. \
                                                      Run `rustc --print tls-models` to \
                                                      see the list of supported values.", s))),
                    }
                    Some(Ok(()))
                })).unwrap_or(Ok(()))
            } );
            ($key_name:ident, PanicStrategy) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                obj.find(&name[..]).and_then(|o| o.as_string().and_then(|s| {
                    match s {
                        "unwind" => base.$key_name = PanicStrategy::Unwind,
                        "abort" => base.$key_name = PanicStrategy::Abort,
                        _ => return Some(Err(format!("'{}' is not a valid value for \
                                                      panic-strategy. Use 'unwind' or 'abort'.",
                                                     s))),
                }
                Some(Ok(()))
            })).unwrap_or(Ok(()))
            } );
            ($key_name:ident, RelroLevel) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                obj.find(&name[..]).and_then(|o| o.as_string().and_then(|s| {
                    match s.parse::<RelroLevel>() {
                        Ok(level) => base.$key_name = level,
                        _ => return Some(Err(format!("'{}' is not a valid value for \
                                                      relro-level. Use 'full', 'partial, or 'off'.",
                                                      s))),
                    }
                    Some(Ok(()))
                })).unwrap_or(Ok(()))
            } );
            ($key_name:ident, SplitDebuginfo) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                obj.find(&name[..]).and_then(|o| o.as_string().and_then(|s| {
                    match s.parse::<SplitDebuginfo>() {
                        Ok(level) => base.$key_name = level,
                        _ => return Some(Err(format!("'{}' is not a valid value for \
                                                      split-debuginfo. Use 'off' or 'dsymutil'.",
                                                      s))),
                    }
                    Some(Ok(()))
                })).unwrap_or(Ok(()))
            } );
            ($key_name:ident, list) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                if let Some(v) = obj.find(&name).and_then(Json::as_array) {
                    base.$key_name = v.iter()
                        .map(|a| a.as_string().unwrap().to_string())
                        .collect();
                }
            } );
            ($key_name:ident, opt_list) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                if let Some(v) = obj.find(&name).and_then(Json::as_array) {
                    base.$key_name = Some(v.iter()
                        .map(|a| a.as_string().unwrap().to_string())
                        .collect());
                }
            } );
            ($key_name:ident, optional) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                if let Some(o) = obj.find(&name[..]) {
                    base.$key_name = o
                        .as_string()
                        .map(|s| s.to_string() );
                }
            } );
            ($key_name:ident = $json_name:expr, optional) => ( {
                let name = $json_name;
                if let Some(o) = obj.find(name) {
                    base.$key_name = o
                        .as_string()
                        .map(|s| s.to_string() );
                }
            } );
            ($key_name:ident, LldFlavor) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                obj.find(&name[..]).and_then(|o| o.as_string().and_then(|s| {
                    if let Some(flavor) = LldFlavor::from_str(&s) {
                        base.$key_name = flavor;
                    } else {
                        return Some(Err(format!(
                            "'{}' is not a valid value for lld-flavor. \
                             Use 'darwin', 'gnu', 'link' or 'wasm.",
                            s)))
                    }
                    Some(Ok(()))
                })).unwrap_or(Ok(()))
            } );
            ($key_name:ident, LinkerFlavor) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                obj.find(&name[..]).and_then(|o| o.as_string().and_then(|s| {
                    match LinkerFlavor::from_str(s) {
                        Some(linker_flavor) => base.$key_name = linker_flavor,
                        _ => return Some(Err(format!("'{}' is not a valid value for linker-flavor. \
                                                      Use {}", s, LinkerFlavor::one_of()))),
                    }
                    Some(Ok(()))
                })).unwrap_or(Ok(()))
            } );
            ($key_name:ident, StackProbeType) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                obj.find(&name[..]).and_then(|o| match StackProbeType::from_json(o) {
                    Ok(v) => {
                        base.$key_name = v;
                        Some(Ok(()))
                    },
                    Err(s) => Some(Err(
                        format!("`{:?}` is not a valid value for `{}`: {}", o, name, s)
                    )),
                }).unwrap_or(Ok(()))
            } );
            ($key_name:ident, crt_objects_fallback) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                obj.find(&name[..]).and_then(|o| o.as_string().and_then(|s| {
                    match s.parse::<CrtObjectsFallback>() {
                        Ok(fallback) => base.$key_name = Some(fallback),
                        _ => return Some(Err(format!("'{}' is not a valid CRT objects fallback. \
                                                      Use 'musl', 'mingw' or 'wasm'", s))),
                    }
                    Some(Ok(()))
                })).unwrap_or(Ok(()))
            } );
            ($key_name:ident, link_objects) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                if let Some(val) = obj.find(&name[..]) {
                    let obj = val.as_object().ok_or_else(|| format!("{}: expected a \
                        JSON object with fields per CRT object kind.", name))?;
                    let mut args = CrtObjects::new();
                    for (k, v) in obj {
                        let kind = LinkOutputKind::from_str(&k).ok_or_else(|| {
                            format!("{}: '{}' is not a valid value for CRT object kind. \
                                     Use '(dynamic,static)-(nopic,pic)-exe' or \
                                     '(dynamic,static)-dylib' or 'wasi-reactor-exe'", name, k)
                        })?;

                        let v = v.as_array().ok_or_else(||
                            format!("{}.{}: expected a JSON array", name, k)
                        )?.iter().enumerate()
                            .map(|(i,s)| {
                                let s = s.as_string().ok_or_else(||
                                    format!("{}.{}[{}]: expected a JSON string", name, k, i))?;
                                Ok(s.to_owned())
                            })
                            .collect::<Result<Vec<_>, String>>()?;

                        args.insert(kind, v);
                    }
                    base.$key_name = args;
                }
            } );
            ($key_name:ident, link_args) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                if let Some(val) = obj.find(&name[..]) {
                    let obj = val.as_object().ok_or_else(|| format!("{}: expected a \
                        JSON object with fields per linker-flavor.", name))?;
                    let mut args = LinkArgs::new();
                    for (k, v) in obj {
                        let flavor = LinkerFlavor::from_str(&k).ok_or_else(|| {
                            format!("{}: '{}' is not a valid value for linker-flavor. \
                                     Use 'em', 'gcc', 'ld' or 'msvc'", name, k)
                        })?;

                        let v = v.as_array().ok_or_else(||
                            format!("{}.{}: expected a JSON array", name, k)
                        )?.iter().enumerate()
                            .map(|(i,s)| {
                                let s = s.as_string().ok_or_else(||
                                    format!("{}.{}[{}]: expected a JSON string", name, k, i))?;
                                Ok(s.to_owned())
                            })
                            .collect::<Result<Vec<_>, String>>()?;

                        args.insert(flavor, v);
                    }
                    base.$key_name = args;
                }
            } );
            ($key_name:ident, env) => ( {
                let name = (stringify!($key_name)).replace("_", "-");
                if let Some(a) = obj.find(&name[..]).and_then(|o| o.as_array()) {
                    for o in a {
                        if let Some(s) = o.as_string() {
                            let p = s.split('=').collect::<Vec<_>>();
                            if p.len() == 2 {
                                let k = p[0].to_string();
                                let v = p[1].to_string();
                                base.$key_name.push((k, v));
                            }
                        }
                    }
                }
            } );
        }

        if let Some(s) = obj.find("target-endian").and_then(Json::as_string) {
            base.endian = s.parse()?;
        }
        key!(is_builtin, bool);
        key!(c_int_width = "target-c-int-width");
        key!(os);
        key!(env);
        key!(vendor);
        key!(linker_flavor, LinkerFlavor)?;
        key!(linker, optional);
        key!(lld_flavor, LldFlavor)?;
        key!(pre_link_objects, link_objects);
        key!(post_link_objects, link_objects);
        key!(pre_link_objects_fallback, link_objects);
        key!(post_link_objects_fallback, link_objects);
        key!(crt_objects_fallback, crt_objects_fallback)?;
        key!(pre_link_args, link_args);
        key!(late_link_args, link_args);
        key!(late_link_args_dynamic, link_args);
        key!(late_link_args_static, link_args);
        key!(post_link_args, link_args);
        key!(link_script, optional);
        key!(link_env, env);
        key!(link_env_remove, list);
        key!(asm_args, list);
        key!(cpu);
        key!(features);
        key!(dynamic_linking, bool);
        key!(only_cdylib, bool);
        key!(executables, bool);
        key!(relocation_model, RelocModel)?;
        key!(code_model, CodeModel)?;
        key!(tls_model, TlsModel)?;
        key!(disable_redzone, bool);
        key!(eliminate_frame_pointer, bool);
        key!(function_sections, bool);
        key!(dll_prefix);
        key!(dll_suffix);
        key!(exe_suffix);
        key!(staticlib_prefix);
        key!(staticlib_suffix);
        key!(os_family = "target-family", optional);
        key!(abi_return_struct_as_int, bool);
        key!(is_like_osx, bool);
        key!(is_like_solaris, bool);
        key!(is_like_windows, bool);
        key!(is_like_msvc, bool);
        key!(is_like_emscripten, bool);
        key!(is_like_fuchsia, bool);
        key!(dwarf_version, Option<u32>);
        key!(linker_is_gnu, bool);
        key!(allows_weak_linkage, bool);
        key!(has_rpath, bool);
        key!(no_default_libraries, bool);
        key!(position_independent_executables, bool);
        key!(static_position_independent_executables, bool);
        key!(needs_plt, bool);
        key!(relro_level, RelroLevel)?;
        key!(archive_format);
        key!(allow_asm, bool);
        key!(main_needs_argc_argv, bool);
        key!(has_elf_tls, bool);
        key!(obj_is_bitcode, bool);
        key!(forces_embed_bitcode, bool);
        key!(bitcode_llvm_cmdline);
        key!(max_atomic_width, Option<u64>);
        key!(min_atomic_width, Option<u64>);
        key!(atomic_cas, bool);
        key!(panic_strategy, PanicStrategy)?;
        key!(crt_static_allows_dylibs, bool);
        key!(crt_static_default, bool);
        key!(crt_static_respected, bool);
        key!(stack_probes, StackProbeType)?;
        key!(min_global_align, Option<u64>);
        key!(default_codegen_units, Option<u64>);
        key!(trap_unreachable, bool);
        key!(requires_lto, bool);
        key!(singlethread, bool);
        key!(no_builtins, bool);
        key!(default_hidden_visibility, bool);
        key!(emit_debug_gdb_scripts, bool);
        key!(requires_uwtable, bool);
        key!(default_uwtable, bool);
        key!(simd_types_indirect, bool);
        key!(limit_rdylib_exports, bool);
        key!(override_export_symbols, opt_list);
        key!(merge_functions, MergeFunctions)?;
        key!(mcount = "target-mcount");
        key!(llvm_abiname);
        key!(relax_elf_relocations, bool);
        key!(llvm_args, list);
        key!(use_ctors_section, bool);
        key!(eh_frame_header, bool);
        key!(has_thumb_interworking, bool);
        key!(split_debuginfo, SplitDebuginfo)?;

        // NB: The old name is deprecated, but support for it is retained for
        // compatibility.
        for name in ["abi-blacklist", "unsupported-abis"].iter() {
            if let Some(array) = obj.find(name).and_then(Json::as_array) {
                for name in array.iter().filter_map(|abi| abi.as_string()) {
                    match lookup_abi(name) {
                        Some(abi) => {
                            if abi.generic() {
                                return Err(format!(
                                    "The ABI \"{}\" is considered to be supported on all \
                                    targets and cannot be marked unsupported",
                                    abi
                                ));
                            }

                            base.unsupported_abis.push(abi)
                        }
                        None => {
                            return Err(format!(
                                "Unknown ABI \"{}\" in target specification",
                                name
                            ));
                        }
                    }
                }
            }
        }

        Ok(base)
    }

    /// Search RUST_TARGET_PATH for a JSON file specifying the given target
    /// triple. Note that it could also just be a bare filename already, so also
    /// check for that. If one of the hardcoded targets we know about, just
    /// return it directly.
    ///
    /// The error string could come from any of the APIs called, including
    /// filesystem access and JSON decoding.
    pub fn search(target_triple: &TargetTriple) -> Result<Target, String> {
        use rustc_serialize::json;
        use std::env;
        use std::fs;

        fn load_file(path: &Path) -> Result<Target, String> {
            let contents = fs::read(path).map_err(|e| e.to_string())?;
            let obj = json::from_reader(&mut &contents[..]).map_err(|e| e.to_string())?;
            Target::from_json(obj)
        }

        match *target_triple {
            TargetTriple::TargetTriple(ref target_triple) => {
                // check if triple is in list of built-in targets
                if let Some(t) = load_builtin(target_triple) {
                    return Ok(t);
                }

                // search for a file named `target_triple`.json in RUST_TARGET_PATH
                let path = {
                    let mut target = target_triple.to_string();
                    target.push_str(".json");
                    PathBuf::from(target)
                };

                let target_path = env::var_os("RUST_TARGET_PATH").unwrap_or_default();

                // FIXME 16351: add a sane default search path?

                for dir in env::split_paths(&target_path) {
                    let p = dir.join(&path);
                    if p.is_file() {
                        return load_file(&p);
                    }
                }
                Err(format!("Could not find specification for target {:?}", target_triple))
            }
            TargetTriple::TargetPath(ref target_path) => {
                if target_path.is_file() {
                    return load_file(&target_path);
                }
                Err(format!("Target path {:?} is not a valid file", target_path))
            }
        }
    }
}

impl ToJson for Target {
    fn to_json(&self) -> Json {
        let mut d = BTreeMap::new();
        let default: TargetOptions = Default::default();

        macro_rules! target_val {
            ($attr:ident) => {{
                let name = (stringify!($attr)).replace("_", "-");
                d.insert(name, self.$attr.to_json());
            }};
            ($attr:ident, $key_name:expr) => {{
                let name = $key_name;
                d.insert(name.to_string(), self.$attr.to_json());
            }};
        }

        macro_rules! target_option_val {
            ($attr:ident) => {{
                let name = (stringify!($attr)).replace("_", "-");
                if default.$attr != self.$attr {
                    d.insert(name, self.$attr.to_json());
                }
            }};
            ($attr:ident, $key_name:expr) => {{
                let name = $key_name;
                if default.$attr != self.$attr {
                    d.insert(name.to_string(), self.$attr.to_json());
                }
            }};
            (link_args - $attr:ident) => {{
                let name = (stringify!($attr)).replace("_", "-");
                if default.$attr != self.$attr {
                    let obj = self
                        .$attr
                        .iter()
                        .map(|(k, v)| (k.desc().to_owned(), v.clone()))
                        .collect::<BTreeMap<_, _>>();
                    d.insert(name, obj.to_json());
                }
            }};
            (env - $attr:ident) => {{
                let name = (stringify!($attr)).replace("_", "-");
                if default.$attr != self.$attr {
                    let obj = self
                        .$attr
                        .iter()
                        .map(|&(ref k, ref v)| k.clone() + "=" + &v)
                        .collect::<Vec<_>>();
                    d.insert(name, obj.to_json());
                }
            }};
        }

        target_val!(llvm_target);
        d.insert("target-pointer-width".to_string(), self.pointer_width.to_string().to_json());
        target_val!(arch);
        target_val!(data_layout);

        target_option_val!(is_builtin);
        target_option_val!(endian, "target-endian");
        target_option_val!(c_int_width, "target-c-int-width");
        target_option_val!(os);
        target_option_val!(env);
        target_option_val!(vendor);
        target_option_val!(linker_flavor);
        target_option_val!(linker);
        target_option_val!(lld_flavor);
        target_option_val!(pre_link_objects);
        target_option_val!(post_link_objects);
        target_option_val!(pre_link_objects_fallback);
        target_option_val!(post_link_objects_fallback);
        target_option_val!(crt_objects_fallback);
        target_option_val!(link_args - pre_link_args);
        target_option_val!(link_args - late_link_args);
        target_option_val!(link_args - late_link_args_dynamic);
        target_option_val!(link_args - late_link_args_static);
        target_option_val!(link_args - post_link_args);
        target_option_val!(link_script);
        target_option_val!(env - link_env);
        target_option_val!(link_env_remove);
        target_option_val!(asm_args);
        target_option_val!(cpu);
        target_option_val!(features);
        target_option_val!(dynamic_linking);
        target_option_val!(only_cdylib);
        target_option_val!(executables);
        target_option_val!(relocation_model);
        target_option_val!(code_model);
        target_option_val!(tls_model);
        target_option_val!(disable_redzone);
        target_option_val!(eliminate_frame_pointer);
        target_option_val!(function_sections);
        target_option_val!(dll_prefix);
        target_option_val!(dll_suffix);
        target_option_val!(exe_suffix);
        target_option_val!(staticlib_prefix);
        target_option_val!(staticlib_suffix);
        target_option_val!(os_family, "target-family");
        target_option_val!(abi_return_struct_as_int);
        target_option_val!(is_like_osx);
        target_option_val!(is_like_solaris);
        target_option_val!(is_like_windows);
        target_option_val!(is_like_msvc);
        target_option_val!(is_like_emscripten);
        target_option_val!(is_like_fuchsia);
        target_option_val!(dwarf_version);
        target_option_val!(linker_is_gnu);
        target_option_val!(allows_weak_linkage);
        target_option_val!(has_rpath);
        target_option_val!(no_default_libraries);
        target_option_val!(position_independent_executables);
        target_option_val!(static_position_independent_executables);
        target_option_val!(needs_plt);
        target_option_val!(relro_level);
        target_option_val!(archive_format);
        target_option_val!(allow_asm);
        target_option_val!(main_needs_argc_argv);
        target_option_val!(has_elf_tls);
        target_option_val!(obj_is_bitcode);
        target_option_val!(forces_embed_bitcode);
        target_option_val!(bitcode_llvm_cmdline);
        target_option_val!(min_atomic_width);
        target_option_val!(max_atomic_width);
        target_option_val!(atomic_cas);
        target_option_val!(panic_strategy);
        target_option_val!(crt_static_allows_dylibs);
        target_option_val!(crt_static_default);
        target_option_val!(crt_static_respected);
        target_option_val!(stack_probes);
        target_option_val!(min_global_align);
        target_option_val!(default_codegen_units);
        target_option_val!(trap_unreachable);
        target_option_val!(requires_lto);
        target_option_val!(singlethread);
        target_option_val!(no_builtins);
        target_option_val!(default_hidden_visibility);
        target_option_val!(emit_debug_gdb_scripts);
        target_option_val!(requires_uwtable);
        target_option_val!(default_uwtable);
        target_option_val!(simd_types_indirect);
        target_option_val!(limit_rdylib_exports);
        target_option_val!(override_export_symbols);
        target_option_val!(merge_functions);
        target_option_val!(mcount, "target-mcount");
        target_option_val!(llvm_abiname);
        target_option_val!(relax_elf_relocations);
        target_option_val!(llvm_args);
        target_option_val!(use_ctors_section);
        target_option_val!(eh_frame_header);
        target_option_val!(has_thumb_interworking);
        target_option_val!(split_debuginfo);

        if default.unsupported_abis != self.unsupported_abis {
            d.insert(
                "unsupported-abis".to_string(),
                self.unsupported_abis
                    .iter()
                    .map(|&name| Abi::name(name).to_json())
                    .collect::<Vec<_>>()
                    .to_json(),
            );
        }

        Json::Object(d)
    }
}

/// Either a target triple string or a path to a JSON file.
#[derive(PartialEq, Clone, Debug, Hash, Encodable, Decodable)]
pub enum TargetTriple {
    TargetTriple(String),
    TargetPath(PathBuf),
}

impl TargetTriple {
    /// Creates a target triple from the passed target triple string.
    pub fn from_triple(triple: &str) -> Self {
        TargetTriple::TargetTriple(triple.to_string())
    }

    /// Creates a target triple from the passed target path.
    pub fn from_path(path: &Path) -> Result<Self, io::Error> {
        let canonicalized_path = path.canonicalize()?;
        Ok(TargetTriple::TargetPath(canonicalized_path))
    }

    /// Returns a string triple for this target.
    ///
    /// If this target is a path, the file name (without extension) is returned.
    pub fn triple(&self) -> &str {
        match *self {
            TargetTriple::TargetTriple(ref triple) => triple,
            TargetTriple::TargetPath(ref path) => path
                .file_stem()
                .expect("target path must not be empty")
                .to_str()
                .expect("target path must be valid unicode"),
        }
    }

    /// Returns an extended string triple for this target.
    ///
    /// If this target is a path, a hash of the path is appended to the triple returned
    /// by `triple()`.
    pub fn debug_triple(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let triple = self.triple();
        if let TargetTriple::TargetPath(ref path) = *self {
            let mut hasher = DefaultHasher::new();
            path.hash(&mut hasher);
            let hash = hasher.finish();
            format!("{}-{}", triple, hash)
        } else {
            triple.to_owned()
        }
    }
}

impl fmt::Display for TargetTriple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.debug_triple())
    }
}
