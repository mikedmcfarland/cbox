use anyhow::Result;

pub async fn run(
    _name: String,
    _project: Option<String>,
    _tier: Option<String>,
    _branch: Option<String>,
    _shell: bool,
    _claude: bool,
    _attach: bool,
) -> Result<()> {
    anyhow::bail!("create-or-attach is not yet implemented (Phase 2)")
}
