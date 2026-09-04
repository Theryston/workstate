use crate::{application::context::AppContext, error::Result};

pub(crate) async fn run(context: AppContext) -> Result<()> {
    context.preflight()?;
    Ok(())
}
