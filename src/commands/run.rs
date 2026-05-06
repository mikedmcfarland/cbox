use anyhow::Result;

pub async fn run(
    _name: String,
    _project: Option<String>,
    _prompt: String,
    _tier: Option<String>,
) -> Result<()> {
    anyhow::bail!("`cbox run` is not yet implemented (Phase 3)")
}
