//! Support for `-Z offload`: building the `std::offload` device graph as part
//! of a normal host build.
//!
//! An offload build compiles each requested executable three times, in order:
//!
//! 1. [`CompileMode::OffloadHostMetadata`]: a host compile without codegen
//!    that writes the manifest of kernel instantiations the host code launches.
//! 2. [`CompileMode::OffloadDevice`]: the same crate compiled for the device
//!    target, reading the manifest and bundling the kernels into `device.bin`.
//! 3. The regular host unit, which embeds `device.bin`.
//!
//! Dependencies of the device unit are resolved in the
//! [`FeaturesFor::Offload`] context, following the `offload` keys of the
//! manifests (see [`OffloadRequest`]).
//!
//! [`CompileMode::OffloadHostMetadata`]: crate::compiler::CompileMode::OffloadHostMetadata
//! [`CompileMode::OffloadDevice`]: crate::compiler::CompileMode::OffloadDevice
//! [`FeaturesFor::Offload`]: crate::resolver::features::FeaturesFor::Offload
//! [`OffloadRequest`]: crate::workspace::dependency::OffloadRequest

use std::ffi::OsString;

use crate::compiler::unit_dependencies::OffloadUnit;
use crate::compiler::{BuildRunner, CompileTarget, Unit};
use crate::context::GlobalContext;
use crate::util::CargoResult;

/// The cfg set on every unit compiled for the device target.
pub const DEVICE_CFG: &str = "device";

/// The configured device target, if `-Z offload` is enabled and
/// `offload.target` is set in the config.
pub fn device_target(gctx: &GlobalContext) -> CargoResult<Option<CompileTarget>> {
    if !gctx.cli_unstable().offload {
        return Ok(None);
    }
    let Some(target) = gctx.get::<Option<String>>(["offload", "target"])? else {
        return Ok(None);
    };
    Ok(Some(CompileTarget::new(
        &target,
        gctx.cli_unstable().json_target_spec,
    )?))
}

/// The rustc arguments that connect the three passes of an offload build.
///
/// rustc needs absolute paths: the manifest written by the host-metadata pass
/// and read by the device pass, and the device image read by the host pass.
pub fn rustc_args(build_runner: &BuildRunner<'_, '_>, unit: &Unit) -> CargoResult<Vec<OsString>> {
    let files = build_runner.files();
    let deps = build_runner.unit_deps(unit);
    let flag = if unit.mode.is_offload_metadata() {
        let manifest = files.offload_manifest_path(unit);
        format!("offload=HostMetadata={}", manifest.display())
    } else if let Some(OffloadUnit::Device{..}) = unit.offload {
        let Some(host_metadata) = deps
            .iter()
            .find(|dep| dep.unit.mode.is_offload_metadata())
        else {
            anyhow::bail!(
                "internal error: offload device unit for `{}` has no host-metadata dependency",
                unit.target.crate_name()
            );
        };
        let manifest = files.offload_manifest_path(&host_metadata.unit);
        format!("offload=Device={}", manifest.display())
    } else if let Some(device) = deps.iter().find(|dep| matches!(dep.unit.offload, Some(OffloadUnit::Device{..}))) {
        let image = files.offload_device_image_path(&device.unit);
        format!("offload=Host={}", image.display())
    } else if unit.is_offload_host() {
        // TODO: We need an Host for non-bins that accepts no device binary
        format!("offload=Test")
    } else {
        return Ok(Vec::new());
    };
    let mut args: Vec<OsString> = vec![
        "-Z".into(),
        "unstable-options".into(),
        "-Z".into(),
        flag.into(),
        // Kernel symbols must be identical on both sides; the host and
        // device passes use the v0 scheme, which rustc's offload export
        // mangling is defined in terms of.
        "-C".into(),
        "symbol-mangling-version=v0".into(),
    ];
    if let Some(OffloadUnit::Device{..}) = unit.offload {
        // Workaround for a rustc bug: the device pass wraps every kernel in a
        // launcher and recognizes an already wrapped kernel by the *name* of
        // its first parameter. Release builds discard value names, so the
        // kernel is wrapped twice and the runtime's arguments are shifted
        // by one. Keeping value names makes the check work.
        args.push("-Z".into());
        args.push("fewer-names=no".into());
    }
    Ok(args)
}
