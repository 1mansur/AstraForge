use astraforge_core::database::Database;
use astraforge_core::error::Result;
use astraforge_core::index::IndexService;
use astraforge_core::provider::EmbeddingProvider;
use astraforge_core::vector::VectorStore;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
struct TestEmbedding {
    calls: AtomicUsize,
}
impl EmbeddingProvider for TestEmbedding {
    fn identity(&self) -> String {
        "test-only-local-vector".into()
    }
    fn embed(&self, inputs: &[String], _: &AtomicBool) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(inputs.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}
#[test]
fn vector_cache_rejects_stale_deleted_cross_repository_and_sensitive_documents() {
    let temporary = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open(&temporary.path().join("vectors.sqlite")).unwrap());
    let _index = IndexService::new(db.clone()).unwrap();
    let store = VectorStore::new(db.clone()).unwrap();
    db.with(|connection| {connection.execute("INSERT INTO index_files(repository_id,path,content,hash,language,size,parse_errors,generation) VALUES('repo','src/a.ts','export const a=1','hash1','TypeScript',16,0,'test')",[])?;Ok(())}).unwrap();
    let provider = TestEmbedding {
        calls: AtomicUsize::new(0),
    };
    let documents = vec![
        ("src/a.ts".into(), "export const a=1".into(), "hash1".into()),
        (
            ".env".into(),
            "SECRET=never-transmit".into(),
            "secret-hash".into(),
        ),
    ];
    assert_eq!(
        store
            .update("repo", &documents, &provider, &AtomicBool::new(false))
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .update("repo", &documents, &provider, &AtomicBool::new(false))
            .unwrap(),
        0
    );
    assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        store
            .search("repo", &provider.identity(), &[1.0, 0.0], Some("src/"), 10)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .search("other-repo", &provider.identity(), &[1.0, 0.0], None, 10)
        .unwrap()
        .is_empty());
    db.with(|connection| {
        connection.execute("UPDATE index_files SET hash='hash2'", [])?;
        Ok(())
    })
    .unwrap();
    assert!(store
        .search("repo", &provider.identity(), &[1.0, 0.0], None, 10)
        .unwrap()
        .is_empty());
    store.delete("repo", "src").unwrap();
    let remaining = db
        .with(|connection| {
            Ok(
                connection.query_row("SELECT count(*) FROM embeddings", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(remaining, 0);
}
