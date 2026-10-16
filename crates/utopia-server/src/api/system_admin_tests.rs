//! 管理员变更留审计，既有会话也在下一次请求重新检查权限。
use crate::state::AppState;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn admin_changes_are_audited_and_existing_sessions_recheck_permissions() -> anyhow::Result<()>
{
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = sqlx::PgPool::connect(&url).await?;
    utopia_store::db::migrate(&pool).await?;
    let (org, admin, member) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let dir = tempfile::tempdir()?;
    let cfg = utopia_core::config::AppConfig {
        data_dir: dir.path().to_string_lossy().into_owned(),
        ..Default::default()
    };
    let state = AppState::new(
        pool.clone(),
        &cfg,
        Arc::new(utopia_search::SearchIndex::open(
            &dir.path().join("search"),
        )?),
        "test-only".into(),
    );
    let result = async {
        sqlx::raw_sql(&format!(
            "INSERT INTO organizations(id,name) VALUES('{org}','sysadmin-route-test');
             INSERT INTO users(id,org_id,email,password_hash,display_name,is_admin) VALUES
               ('{admin}','{org}','{admin}@example.test','unused','Admin',TRUE),
               ('{member}','{org}','{member}@example.test','unused','Member',FALSE);"
        ))
        .execute(&pool)
        .await?;
        let admin_token = crate::auth::issue_token(&state, admin)?;
        let member_token = crate::auth::issue_token(&state, member)?;
        let change = |token: String, method: Method, is_admin: bool| {
            let state = state.clone();
            async move {
                let request = Request::builder()
                    .method(method)
                    .uri(format!("/api/v1/admin/users/{member}"))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "is_admin": is_admin }).to_string()))?;
                let response = super::router(state, &Default::default())
                    .oneshot(request)
                    .await?;
                anyhow::Ok(response.status())
            }
        };

        anyhow::ensure!(
            change(member_token.clone(), Method::PUT, true).await? == StatusCode::FORBIDDEN
        );
        anyhow::ensure!(change(admin_token.clone(), Method::PUT, true).await? == StatusCode::OK);

        let flag: bool = sqlx::query_scalar("SELECT is_admin FROM users WHERE id = $1")
            .bind(member)
            .fetch_one(&pool)
            .await?;
        let audited: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events
              WHERE actor_id = $1 AND action = 'user.admin_granted' AND target_id = $2",
        )
        .bind(admin)
        .bind(member)
        .fetch_one(&pool)
        .await?;
        anyhow::ensure!(flag && audited == 1, "flag={flag} audited={audited}");
        anyhow::ensure!(change(member_token.clone(), Method::PUT, true).await? == StatusCode::OK);
        anyhow::ensure!(change(admin_token.clone(), Method::PUT, false).await? == StatusCode::OK);
        anyhow::ensure!(
            change(member_token.clone(), Method::PUT, true).await? == StatusCode::FORBIDDEN
        );
        anyhow::ensure!(change(admin_token.clone(), Method::PUT, true).await? == StatusCode::OK);
        anyhow::ensure!(change(admin_token, Method::DELETE, false).await? == StatusCode::OK);
        anyhow::ensure!(change(member_token, Method::PUT, true).await? == StatusCode::UNAUTHORIZED);

        let changes: Vec<(String, i64)> = sqlx::query_as(
            "SELECT action, count(*) FROM audit_events
              WHERE actor_id = $1 AND target_id = $2 GROUP BY action ORDER BY action",
        )
        .bind(admin)
        .bind(member)
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            changes,
            vec![
                ("user.admin_granted".into(), 2),
                ("user.admin_revoked".into(), 1),
                ("user.deactivated".into(), 1),
            ]
        );
        anyhow::Ok(())
    }
    .await;
    sqlx::query("DELETE FROM organizations WHERE id=$1")
        .bind(org)
        .execute(&pool)
        .await?;
    result
}
