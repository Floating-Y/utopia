//! A page imported before nested traversal must gain its missing body on resync,
//! even when Notion's edit timestamp has not changed.
use super::{fetch_pages, Paced};
use crate::ingest_sources::{ingest_item, IngestAction};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::sync::Arc;
use utopia_store::documents;
use uuid::Uuid;
use wiremock::{matchers::method, matchers::path, Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn resync_adds_nested_body_without_a_new_edit_timestamp() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = sqlx::PgPool::connect(&url).await?;
    let (org, workspace, kb, source) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    let result = async {
        sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'notion-resync-test')")
            .bind(org)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO workspaces(id,org_id,name) VALUES($1,$2,'notion-resync-test')")
            .bind(workspace)
            .bind(org)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO knowledge_bases(id,workspace_id,name) VALUES($1,$2,'notion-resync-test')",
        )
        .bind(kb)
        .bind(workspace)
        .execute(&pool)
        .await?;
        sqlx::query("INSERT INTO sources(id,kb_id,kind,name) VALUES($1,$2,'notion','fixture')")
            .bind(source)
            .bind(kb)
            .execute(&pool)
            .await?;
        let dir = tempfile::tempdir()?;
        let config = utopia_core::config::AppConfig {
            data_dir: dir.path().to_string_lossy().into_owned(),
            ..Default::default()
        };
        let search = Arc::new(utopia_search::SearchIndex::open(
            &dir.path().join("search"),
        )?);
        let state = crate::state::AppState::new(pool.clone(), &config, search, "test-only".into());
        let edited: DateTime<Utc> = "2026-10-01T12:00:00Z".parse()?;
        let external_key = "notion://page";
        let old_body = "# Quarterly policy\n\nPolicy details\n";
        let complete_body = "# Quarterly policy\n\nPolicy details\nRevenue was 42 million.\n";

        // Seed exactly the body the old top-level-only reader would have stored.
        let created = ingest_item(
            &state,
            kb,
            source,
            external_key,
            "Quarterly-policy.md",
            "text/markdown",
            old_body.as_bytes(),
            Some(edited),
        )
        .await?;
        anyhow::ensure!(created == IngestAction::Created);
        let original = documents::find_by_external_key(&pool, source, external_key)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the original page was not persisted"))?;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{
                    "id": "page",
                    "properties": {"Name": {"type": "title", "title": [{"plain_text": "Quarterly policy"}]}},
                    "last_edited_time": "2026-10-01T12:00:00Z"
                }],
                "has_more": false
            })))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/blocks/page/children"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{
                    "id": "toggle", "type": "toggle", "has_children": true,
                    "toggle": {"rich_text": [{"plain_text": "Policy details"}]}
                }],
                "has_more": false
            })))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/blocks/toggle/children"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{
                    "id": "paragraph", "type": "paragraph", "has_children": false,
                    "paragraph": {"rich_text": [{"plain_text": "Revenue was 42 million."}]}
                }],
                "has_more": false
            })))
            .expect(2)
            .mount(&server)
            .await;
        let mut http = Paced::new(reqwest::Client::builder().no_proxy().build()?);
        http.api_root = server.uri();

        for expected in [IngestAction::Updated, IngestAction::Unchanged] {
            let (pages, truncated) = fetch_pages(&mut http, None).await?;
            anyhow::ensure!(!truncated && pages.len() == 1);
            let page = &pages[0];
            anyhow::ensure!(page.external_key == external_key);
            anyhow::ensure!(page.last_edited == Some(edited));
            anyhow::ensure!(page.text == complete_body);
            let action = ingest_item(
                &state,
                kb,
                source,
                &page.external_key,
                &page.filename,
                "text/markdown",
                page.text.as_bytes(),
                page.last_edited,
            )
            .await?;
            anyhow::ensure!(action == expected, "expected {expected:?}, got {action:?}");
            let updated = documents::find_by_external_key(&pool, source, external_key)
                .await?
                .ok_or_else(|| anyhow::anyhow!("the resynced page was not persisted"))?;
            anyhow::ensure!(updated.id == original.id);
            anyhow::ensure!(updated.external_key.as_deref() == Some(external_key));
            anyhow::ensure!(updated.doc_time == original.doc_time);
            anyhow::ensure!(updated.doc_time == Some(edited));
            anyhow::ensure!(updated.sha256 != original.sha256);
            anyhow::ensure!(state.blob.get(&updated.sha256).await? == complete_body.as_bytes());
            anyhow::ensure!(state.blob.get(&original.sha256).await? == old_body.as_bytes());
            let versions: Vec<(i32, String)> = sqlx::query_as(
                "SELECT version, sha256 FROM document_versions WHERE document_id=$1 ORDER BY version",
            )
            .bind(updated.id)
            .fetch_all(&pool)
            .await?;
            anyhow::ensure!(
                versions == vec![(1, original.sha256.clone()), (2, updated.sha256)],
                "resync must retain the old body and create exactly one new version"
            );
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    sqlx::query("DELETE FROM knowledge_bases WHERE id=$1")
        .bind(kb)
        .execute(&pool)
        .await?;
    sqlx::query("DELETE FROM organizations WHERE id=$1")
        .bind(org)
        .execute(&pool)
        .await?;
    result
}
