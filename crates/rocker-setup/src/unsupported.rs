//! Fallback for platforms without a desktop-integration implementation. The
//! binary still runs; only the `install`/`uninstall`/`doctor` verbs are inert.

use crate::{Diagnosis, InstallOptions, Report, UninstallOptions};

fn unsupported() -> anyhow::Error {
    anyhow::anyhow!(
        "self-install isn't implemented for this platform yet — place the `rocker` \
         binary on your PATH manually"
    )
}

pub(crate) fn install(_: &InstallOptions, _: &mut Report) -> anyhow::Result<()> {
    Err(unsupported())
}

pub(crate) fn uninstall(_: &UninstallOptions, _: &mut Report) -> anyhow::Result<()> {
    Err(unsupported())
}

pub(crate) fn doctor() -> anyhow::Result<Diagnosis> {
    Err(unsupported())
}
