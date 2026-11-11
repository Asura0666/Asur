use anyhow::Result;
use clap::Args;

use crate::ui;

#[derive(Args)]
pub struct DoctorArgs {}

pub async fn run(_args: DoctorArgs) -> Result<()> {
    ui::not_implemented("asur doctor", "stretch goal");
    Ok(())
}
