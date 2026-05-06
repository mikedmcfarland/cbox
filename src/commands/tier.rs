use anyhow::Result;

use crate::cli::TierOp;

pub async fn run(op: TierOp) -> Result<()> {
    match op {
        TierOp::Stop { .. } => anyhow::bail!("`cbox tier stop` is not yet implemented (Phase 5)"),
        TierOp::Pause { .. } => anyhow::bail!("`cbox tier pause` is not yet implemented (Phase 5)"),
        TierOp::Resume { .. } => anyhow::bail!("`cbox tier resume` is not yet implemented (Phase 5)"),
        TierOp::List => anyhow::bail!("`cbox tier list` is not yet implemented (Phase 3)"),
    }
}
