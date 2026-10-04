use super::{token_expires_at, IssueReq};
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use utopia_core::AppError;
use uuid::Uuid;

#[test]
fn omitted_expiration_defaults_to_ninety_days() {
    let request: IssueReq = serde_json::from_value(json!({ "name": "laptop" })).unwrap();
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();

    assert_eq!(request.expires_in_days, 90);
    assert_eq!(
        token_expires_at(now, request.expires_in_days).unwrap(),
        Some(Utc.with_ymd_and_hms(2026, 4, 1, 12, 0, 0).unwrap())
    );
}

#[test]
fn zero_is_permanent_and_representable_positive_days_are_accepted() {
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
    assert_eq!(token_expires_at(now, 0).unwrap(), None);

    for days in [1, 365, 1_000_000] {
        let expires_at = token_expires_at(now, days).unwrap().unwrap();
        assert_eq!(expires_at.signed_duration_since(now), Duration::days(days));
    }

    // 上限来自日期能否表示，不是任意的业务天数限制。
    let last_valid_days = DateTime::<Utc>::MAX_UTC
        .signed_duration_since(now)
        .num_days();
    assert!(token_expires_at(now, last_valid_days).unwrap().is_some());
    assert!(token_expires_at(now, last_valid_days + 1).is_err());
}

#[test]
fn negative_days_and_both_overflow_paths_are_validation_errors() {
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
    let date_overflow = Duration::try_days(100_000_000).unwrap();
    assert!(now.checked_add_signed(date_overflow).is_none());
    assert!(Duration::try_days(i64::MAX).is_none());

    for (days, expected_message) in [
        (-1, "expires_in_days must be non-negative"),
        (i64::MIN, "expires_in_days must be non-negative"),
        (100_000_000, "Token expiration date is out of range"),
        (i64::MAX, "Token expiration duration is out of range"),
    ] {
        let error = token_expires_at(now, days).unwrap_err();
        match error {
            AppError::Invalid {
                code,
                message,
                detail,
            } => {
                assert_eq!(code, "bad_token_expiry", "days={days}");
                assert_eq!(message, expected_message, "days={days}");
                assert!(detail.is_none());
            }
            other => panic!("expected validation error for days={days}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn issue_route_validates_expiration_before_writing_tokens_or_success_audits(
) -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = sqlx::PgPool::connect(&url).await?;
    utopia_store::db::migrate(&pool).await?;
    let org_id = Uuid::now_v7();
    let user_id = Uuid::now_v7();
    let directory = tempfile::tempdir()?;
    let config = utopia_core::config::AppConfig {
        data_dir: directory.path().to_string_lossy().into_owned(),
        ..Default::default()
    };
    let search = Arc::new(utopia_search::SearchIndex::open(
        &directory.path().join("search"),
    )?);
    let state = crate::state::AppState::new(pool.clone(), &config, search, "test-only".into());
    let session_token = crate::auth::issue_token(&state, user_id)?;
    let app = crate::api::router(state, &config);

    let result = async {
        sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'token-expiry-test')")
            .bind(org_id)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO users (id, org_id, email, password_hash, display_name)
             VALUES ($1, $2, $3, 'unused', 'Token Expiry Test')",
        )
        .bind(user_id)
        .bind(org_id)
        .bind(format!("{user_id}@token-expiry.test"))
        .execute(&pool)
        .await?;

        let mut issued_count = 0_i64;
        for (requested_days, accepted_days) in [
            (None, Some(90)),
            (Some(0), Some(0)),
            (Some(1), Some(1)),
            (Some(365), Some(365)),
            (Some(-1), None),
            (Some(i64::MIN), None),
            (Some(100_000_000), None),
            (Some(i64::MAX), None),
        ] {
            let mut payload = json!({ "name": "laptop" });
            if let Some(days) = requested_days {
                payload["expires_in_days"] = json!(days);
            }
            let request = Request::builder()
                .method("POST")
                .uri("/api/v1/me/tokens")
                .header("authorization", format!("Bearer {session_token}"))
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))?;
            let before = Utc::now();
            let response = app.clone().oneshot(request).await?;
            let after = Utc::now();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 65536).await?;
            let body: Value = serde_json::from_slice(&bytes)?;

            if let Some(days) = accepted_days {
                anyhow::ensure!(status == StatusCode::OK, "{payload}: {status} {body}");
                anyhow::ensure!(body["token"]
                    .as_str()
                    .is_some_and(|token| token.starts_with("utp_pat_")));
                anyhow::ensure!(body["info"]["scope"] == "read");
                anyhow::ensure!(body["info"]["kb_ids"].is_null());
                let expires_at: Option<DateTime<Utc>> =
                    serde_json::from_value(body["info"]["expires_at"].clone())?;
                if days == 0 {
                    anyhow::ensure!(expires_at.is_none());
                } else {
                    let expires_at =
                        expires_at.ok_or_else(|| anyhow::anyhow!("missing expiration"))?;
                    // PostgreSQL 保留微秒，比较时使用同一精度。
                    let earliest = (before + Duration::days(days)).timestamp_micros();
                    let latest = (after + Duration::days(days)).timestamp_micros();
                    anyhow::ensure!((earliest..=latest).contains(&expires_at.timestamp_micros()));
                }
                issued_count += 1;
            } else {
                anyhow::ensure!(
                    status == StatusCode::UNPROCESSABLE_ENTITY,
                    "{payload}: {status} {body}"
                );
                anyhow::ensure!(body["code"] == "bad_token_expiry");
                anyhow::ensure!(body["error"]
                    .as_str()
                    .is_some_and(|message| !message.is_empty()));
                anyhow::ensure!(body.get("token").is_none());
            }

            let token_count: i64 =
                sqlx::query_scalar("SELECT count(*) FROM personal_tokens WHERE user_id = $1")
                    .bind(user_id)
                    .fetch_one(&pool)
                    .await?;
            let audit_count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM audit_events WHERE actor_id = $1 AND action = 'token.issued'",
            )
            .bind(user_id)
            .fetch_one(&pool)
            .await?;
            anyhow::ensure!(
                token_count == issued_count,
                "unexpected token count for {payload}"
            );
            anyhow::ensure!(
                audit_count == issued_count,
                "unexpected success audit count for {payload}"
            );
        }
        anyhow::Ok(())
    }
    .await;

    // 审计只追加，保留在专用测试库中；随机用户 ID 隔离每次运行的计数。
    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(org_id)
        .execute(&pool)
        .await?;
    result
}
