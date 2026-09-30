use crate::database::Database;
use crate::error::{AppError, Result};
use crate::provider::EmbeddingProvider;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorHit {
    pub path: String,
    pub score: f32,
    pub content: String,
}
pub struct VectorStore {
    db: Arc<Database>,
}
impl VectorStore {
    pub fn new(db: Arc<Database>) -> Result<Self> {
        db.with(|connection| {
            connection.execute_batch("CREATE TABLE IF NOT EXISTS embeddings(repository_id TEXT NOT NULL,path TEXT NOT NULL,provider TEXT NOT NULL,hash TEXT NOT NULL,vector TEXT NOT NULL,content TEXT NOT NULL,PRIMARY KEY(repository_id,path,provider));")?;
            Ok(())
        })?;
        Ok(Self { db })
    }
    pub fn update(
        &self,
        repository: &str,
        documents: &[(String, String, String)],
        provider: &dyn EmbeddingProvider,
        cancel: &AtomicBool,
    ) -> Result<usize> {
        let identity = provider.identity();
        let pending = self.db.with(|connection| {
            let mut pending = Vec::new();
            let mut statement = connection.prepare(
                "SELECT hash FROM embeddings WHERE repository_id=?1 AND path=?2 AND provider=?3",
            )?;
            for (path, content, hash) in documents {
                if !crate::trust::agent_path_allowed(path) {
                    continue;
                }
                let stored = statement.query_row(params![repository, path, identity], |row| {
                    row.get::<_, String>(0)
                });
                match stored {
                    Ok(previous) if previous == *hash => (),
                    Ok(_) | Err(rusqlite::Error::QueryReturnedNoRows) => pending.push((
                        path.clone(),
                        content.chars().take(3000).collect::<String>(),
                        hash.clone(),
                    )),
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(pending)
        })?;
        let mut count = 0;
        for batch in pending.chunks(16) {
            if cancel.load(Ordering::Relaxed) {
                return Err(AppError::new("cancelled", "Embedding indexing cancelled"));
            }
            let vectors = provider.embed(
                &batch.iter().map(|item| item.1.clone()).collect::<Vec<_>>(),
                cancel,
            )?;
            if vectors.len() != batch.len() {
                return Err(AppError::new(
                    "embedding_protocol",
                    "Embedding batch length does not match input",
                ));
            }
            self.db.with(|connection| {
                let transaction = connection.transaction()?;
                for ((path, content, hash), vector) in batch.iter().zip(vectors.iter()) {
                    validate_vector(vector)?;
                    transaction.execute("INSERT OR REPLACE INTO embeddings(repository_id,path,provider,hash,vector,content) VALUES(?1,?2,?3,?4,?5,?6)",params![repository,path,identity,hash,serde_json::to_string(vector)?,content])?;
                }
                transaction.commit()?;
                Ok(())
            })?;
            count += batch.len();
        }
        Ok(count)
    }
    pub fn delete(&self, repository: &str, path: &str) -> Result<()> {
        self.db.with(|connection| { connection.execute("DELETE FROM embeddings WHERE repository_id=?1 AND (path=?2 OR substr(path,1,length(?2)+1)=?2||'/')", params![repository,path])?; Ok(()) })
    }
    pub fn search(
        &self,
        repository: &str,
        provider: &str,
        query: &[f32],
        prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<VectorHit>> {
        validate_vector(query)?;
        self.db.with(|connection| {
            let mut statement = connection.prepare("SELECT e.path,e.vector,e.content FROM embeddings e JOIN index_files f ON f.repository_id=e.repository_id AND f.path=e.path AND f.hash=e.hash WHERE e.repository_id=?1 AND e.provider=?2")?;
            let mut rows = statement.query(params![repository,provider])?;
            let mut hits = Vec::new();
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                if !crate::trust::agent_path_allowed(&path) { continue; }
                if prefix.is_some_and(|filter| !path.starts_with(filter)) { continue; }
                let vector: Vec<f32> = serde_json::from_str(&row.get::<_,String>(1)?)?;
                if vector.len() != query.len() { continue; }
                hits.push(VectorHit { path, score: cosine(query,&vector)?, content: row.get(2)? });
                hits.sort_unstable_by(|left,right| right.score.total_cmp(&left.score));
                hits.truncate(limit.min(100));
            }
            Ok(hits)
        })
    }
}
fn validate_vector(vector: &[f32]) -> Result<()> {
    if vector.is_empty() || vector.len() > 8192 || vector.iter().any(|v| !v.is_finite()) {
        return Err(AppError::new(
            "invalid_vector",
            "Vector is empty, non-finite, or exceeds dimension limit",
        ));
    }
    Ok(())
}
pub fn cosine(a: &[f32], b: &[f32]) -> Result<f32> {
    validate_vector(a)?;
    validate_vector(b)?;
    if a.len() != b.len() {
        return Err(AppError::new(
            "vector_dimensions",
            "Vector dimensions differ",
        ));
    }
    let dot = a
        .iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum::<f64>();
    let aa = a.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>();
    let bb = b.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>();
    Ok(if aa == 0.0 || bb == 0.0 {
        0.0
    } else {
        (dot / (aa * bb).sqrt()).clamp(-1.0, 1.0) as f32
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn similarity_is_bounded_and_rejects_malformed_vectors() {
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0]).unwrap(), 1.0);
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]).unwrap(), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[0.0, 1.0]).unwrap(), 0.0);
        assert!(cosine(&[f32::NAN], &[1.0]).is_err());
        assert!(cosine(&[1.0], &[1.0, 2.0]).is_err());
    }
    proptest::proptest! {
        #[test]
        fn cosine_identity(x in -1000f32..1000f32,y in -1000f32..1000f32) {
            let score = cosine(&[x,y], &[x,y]).unwrap();
            proptest::prop_assert!((-1.0..=1.0).contains(&score));
            if x != 0.0 || y != 0.0 { proptest::prop_assert!((score-1.0).abs()<0.0001); }
        }
    }
}
