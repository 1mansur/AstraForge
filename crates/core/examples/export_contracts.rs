use astraforge_core::agent::ToolCall;
use astraforge_core::service::Engine;
use std::sync::Arc;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args()
        .nth(1)
        .ok_or("Pass an output documentation directory")?;
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory)?;
    let temp = tempfile::tempdir()?;
    let engine = Engine::new(&temp.path().join("schema.sqlite"), Arc::new(|_, _| {}))?;
    let schema=engine.db.with(|connection| {
        let mut statement=connection.prepare("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'index_text_%' ORDER BY type DESC,name")?;
        let rows=statement.query_map([],|row|row.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        Ok(rows.join(";\n")+";\n")
    })?;
    std::fs::write(directory.join("schema.sql"), schema)?;
    std::fs::write(
        directory.join("ai-tools.schema.json"),
        serde_json::to_string_pretty(&schemars::schema_for!(ToolCall))?,
    )?;
    Ok(())
}
