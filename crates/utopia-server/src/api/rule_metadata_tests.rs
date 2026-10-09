//! Partial rule patches must preserve retained definition fields and validate the resulting rule.
use crate::state::AppState;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn metadata_patch_preserves_computed_definition_through_authenticated_routes(
) -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = sqlx::PgPool::connect(&url).await?;
    utopia_store::db::migrate(&pool).await?;
    let (org, ws, kb, user, class, input, output) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
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
        sqlx::raw_sql(&format!("INSERT INTO organizations(id,name) VALUES('{org}','metadata-test');
        INSERT INTO workspaces(id,org_id,name) VALUES('{ws}','{org}','metadata-test');
        INSERT INTO knowledge_bases(id,workspace_id,name) VALUES('{kb}','{ws}','metadata-test');
        INSERT INTO users(id,org_id,email,password_hash,display_name) VALUES('{user}','{org}','{user}@example.test','unused','Test');
        INSERT INTO kb_members(kb_id,user_id,role) VALUES('{kb}','{user}','editor');
        INSERT INTO entity_types(id,kb_id,key,label) VALUES('{class}','{kb}','thing','Thing');
        INSERT INTO relation_types(id,kb_id,key,label,kind,datatype) VALUES('{input}','{kb}','input','Input','attribute','number'),('{output}','{kb}','output','Output','attribute','number');"))
            .execute(&pool).await?;
        let token = crate::auth::issue_token(&state, user)?;
        let call = |method: &'static str, path: String, body: Value| {
            let state = state.clone(); let token = token.clone();
            async move {
                let request = Request::builder().method(method).uri(path).header("authorization", format!("Bearer {token}"))
                    .header("content-type","application/json").body(Body::from(body.to_string()))?;
                let response = super::router(state, &Default::default()).oneshot(request).await?;
                let status = response.status();
                let body = axum::body::to_bytes(response.into_body(), 65536).await?;
                anyhow::Ok((status, serde_json::from_slice::<Value>(&body)?))
            }
        };
        let base = format!("/api/v1/kbs/{kb}/rules");
        let expr = json!({"op":"div","l":{"op":"sub","l":{"attr":input},"r":{"const":2}},"r":{"attr":input}});
        let definition = json!({"name":"Original", "subject_type_id":class, "conclusion":"computed", "conclude_predicate_id":output,"conclude_expr":expr,"conditions":[{"predicate_id":input,"op":"gt","operand":3,"group":2},{"predicate_id":input,"op":"lt","operand":-1,"group":7}]});
        let (status, created) = call("POST",base.clone(),definition.clone()).await?;
        anyhow::ensure!(status.is_success(), "create: {status} {created}");
        let id = created["id"].as_str().unwrap();
        let (_, before) = call("GET",base.clone(),json!(null)).await?;
        let snapshot = |value: &Value| { let r=&value["rules"][0]; json!({"expr":r["conclude_expr"],"conditions":r["conditions"],"conclusion":r["conclusion"],"predicate":r["conclude_predicate_id"],"subject":r["subject_type_id"]}) };
        let path = format!("{base}/{id}");
        // The previous form sent the entire conclusion without its expression.
        let mut old_editor = definition;
        old_editor.as_object_mut().unwrap().remove("conclude_expr");
        let (old_status, old_error) = call("PATCH", path.clone(), old_editor).await?;
        anyhow::ensure!(old_status == StatusCode::UNPROCESSABLE_ENTITY);
        anyhow::ensure!(old_error["code"] == "no_expression");

        let (status, _) = call("PATCH", path.clone(), json!({"name":"Renamed","description":"Only metadata"})).await?;
        anyhow::ensure!(status == StatusCode::OK);
        let (_, after) = call("GET",base.clone(),json!(null)).await?;
        anyhow::ensure!(snapshot(&before) == snapshot(&after));
        anyhow::ensure!(after["rules"][0]["name"] == "Renamed");
        // Rejecting a name or permission never writes a partial definition.
        anyhow::ensure!(call("PATCH",path.clone(),json!({"name":" "})).await?.0 == StatusCode::UNPROCESSABLE_ENTITY);
        sqlx::query("UPDATE kb_members SET role='viewer' WHERE user_id=$1").bind(user).execute(&pool).await?;
        anyhow::ensure!(call("PATCH",path,json!({"name":"Forbidden"})).await?.0 == StatusCode::FORBIDDEN);
        let (_, unchanged) = call("GET",base,json!(null)).await?;
        anyhow::ensure!(unchanged == after);
        anyhow::Ok(())
    }.await;
    sqlx::query("DELETE FROM organizations WHERE id=$1")
        .bind(org)
        .execute(&pool)
        .await?;
    result
}

#[tokio::test]
async fn conclusion_only_patch_rejects_retained_y_conditions_through_authenticated_routes(
) -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = sqlx::PgPool::connect(&url).await?;
    utopia_store::db::migrate(&pool).await?;
    let (org, workspace, kb, user) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    let subject_type = Uuid::now_v7();
    let (x, y) = (Uuid::now_v7(), Uuid::now_v7());
    let (pressure, flag, supplies, upstream_of) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    let dir = tempfile::tempdir()?;
    let cfg = utopia_core::config::AppConfig {
        data_dir: dir.path().to_string_lossy().into_owned(),
        ..Default::default()
    };
    let result = async {
        // Only locally generated UUIDs are interpolated into fixture SQL.
        sqlx::raw_sql(&format!(
            "INSERT INTO organizations(id,name) VALUES('{org}','rule-patch-test');
             INSERT INTO workspaces(id,org_id,name) VALUES('{workspace}','{org}','rule-patch-test');
             INSERT INTO knowledge_bases(id,workspace_id,name) VALUES('{kb}','{workspace}','rule-patch-test');
             INSERT INTO users(id,org_id,email,password_hash,display_name)
                 VALUES('{user}','{org}','{user}@example.test','unused','Editor');
             INSERT INTO kb_members(kb_id,user_id,role) VALUES('{kb}','{user}','editor');
             INSERT INTO entity_types(id,kb_id,key,label) VALUES('{subject_type}','{kb}','thing','Thing');
             INSERT INTO entities(id,kb_id,type_id,canonical_name) VALUES
                 ('{x}','{kb}','{subject_type}','X'), ('{y}','{kb}','{subject_type}','Y');
             INSERT INTO relation_types(id,kb_id,key,label,kind,datatype) VALUES
                 ('{pressure}','{kb}','pressure','Pressure','attribute','number'),
                 ('{flag}','{kb}','flag','Flag','attribute','bool');
             INSERT INTO relation_types(id,kb_id,key,label,kind) VALUES
                 ('{supplies}','{kb}','supplies','Supplies','relation'),
                 ('{upstream_of}','{kb}','upstream_of','Upstream of','relation');"
        ))
        .execute(&pool)
        .await?;
        for (subject, value) in [(x, 120), (y, 10)] {
            sqlx::query(
                "INSERT INTO facts(id,kb_id,subject_id,predicate_id,object_value,
                                   valid_from,valid_from_precision,confidence)
                 VALUES($1,$2,$3,$4,$5,'2024-01-01','day',0.9)",
            )
            .bind(Uuid::now_v7())
            .bind(kb)
            .bind(subject)
            .bind(pressure)
            .bind(json!({ "value": value }))
            .execute(&pool)
            .await?;
        }
        sqlx::query(
            "INSERT INTO facts(id,kb_id,subject_id,predicate_id,object_id,
                               valid_from,valid_from_precision,confidence)
             VALUES($1,$2,$3,$4,$5,'2024-01-01','day',0.9)",
        )
        .bind(Uuid::now_v7())
        .bind(kb)
        .bind(x)
        .bind(supplies)
        .bind(y)
        .execute(&pool)
        .await?;

        let state = AppState::new(
            pool.clone(),
            &cfg,
            Arc::new(utopia_search::SearchIndex::open(&dir.path().join("search"))?),
            "test-only".into(),
        );
        let token = crate::auth::issue_token(&state, user)?;
        let app = super::router(state, &cfg);
        let call = |method: &'static str, path: String, body: Value| {
            let app = app.clone();
            let token = token.clone();
            async move {
                let request = Request::builder()
                    .method(method)
                    .uri(path)
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))?;
                let response = app.oneshot(request).await?;
                let status = response.status();
                let body = axum::body::to_bytes(response.into_body(), 65536).await?;
                anyhow::Ok((status, serde_json::from_slice::<Value>(&body)?))
            }
        };
        let base = format!("/api/v1/kbs/{kb}/rules");
        let run_path = format!("{base}/run");
        let y_conditions = json!([{
            "predicate_id": pressure, "op": "gt", "operand": 80, "side": "y"
        }]);
        let (status, created) = call(
            "POST",
            base.clone(),
            json!({
                "name": "Read Y pressure", "subject_type_id": subject_type,
                "conclusion": "relation", "conclude_predicate_id": upstream_of,
                "join_predicate_id": supplies, "conditions": y_conditions
            }),
        )
        .await?;
        anyhow::ensure!(status == StatusCode::OK, "create: {status} {created}");
        let rule_id: Uuid = serde_json::from_value(created["id"].clone())?;
        let patch_path = format!("{base}/{rule_id}");
        let (status, report) = call("POST", run_path.clone(), json!({})).await?;
        anyhow::ensure!(status == StatusCode::OK, "initial run: {status} {report}");
        anyhow::ensure!(report["hits"] == 0, "Y.pressure = 10 does not exceed 80: {report}");

        let saved_rule = || async {
            sqlx::query_scalar::<_, Value>(
                "SELECT jsonb_build_object(
                     'rule', to_jsonb(r),
                     'conditions', (SELECT jsonb_agg(to_jsonb(c) ORDER BY group_seq, seq)
                                      FROM attribute_rule_conditions c WHERE c.rule_id = r.id),
                     'versions', (SELECT jsonb_agg(to_jsonb(v) ORDER BY seq)
                                    FROM attribute_rule_versions v WHERE v.rule_id = r.id))
                 FROM attribute_rules r WHERE r.kb_id = $1 AND r.id = $2",
            )
            .bind(kb)
            .bind(rule_id)
            .fetch_one(&pool)
            .await
        };
        let before = saved_rule().await?;
        let conclusion_patch = json!({
            "conclusion": "attribute", "conclude_predicate_id": flag, "conclude_value": true
        });
        // These omissions must reach the store as retained conditions and a cleared join.
        anyhow::ensure!(conclusion_patch.get("conditions").is_none(), "the reproduction must retain conditions");
        anyhow::ensure!(conclusion_patch.get("join_predicate_id").is_none(), "the reproduction must omit the join");
        let (status, error) = call("PATCH", patch_path.clone(), conclusion_patch.clone()).await?;
        anyhow::ensure!(status == StatusCode::UNPROCESSABLE_ENTITY, "patch: {status} {error}");
        anyhow::ensure!(error["code"] == "condition_side_without_join", "patch error: {error}");
        anyhow::ensure!(saved_rule().await? == before, "a rejected HTTP patch must not write any part of the rule");

        let (status, listed) = call("GET", base.clone(), Value::Null).await?;
        anyhow::ensure!(status == StatusCode::OK, "list: {status} {listed}");
        let retained = &listed["rules"][0];
        anyhow::ensure!(retained["conclusion"] == "relation", "retained conclusion: {retained}");
        anyhow::ensure!(retained["join_predicate_id"] == json!(supplies), "retained join: {retained}");
        anyhow::ensure!(retained["conditions"][0]["side"] == "y", "retained conditions: {retained}");
        let (status, report) = call("POST", run_path.clone(), json!({})).await?;
        anyhow::ensure!(status == StatusCode::OK, "run after rejection: {status} {report}");
        anyhow::ensure!(report["hits"] == 0, "rejected patch still has no matches: {report}");
        let wrong_flags: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM derived_facts
             WHERE kb_id = $1 AND predicate_id = $2 AND invalidated_at IS NULL",
        )
        .bind(kb)
        .bind(flag)
        .fetch_one(&pool)
        .await?;
        anyhow::ensure!(wrong_flags == 0, "the rejected patch cannot derive X.flag from X.pressure");

        let mut valid_patch = conclusion_patch;
        valid_patch["conditions"] = json!([{
            "predicate_id": pressure, "op": "gt", "operand": 80, "side": "x"
        }]);
        sqlx::query("UPDATE kb_members SET role = 'viewer' WHERE kb_id = $1 AND user_id = $2")
            .bind(kb)
            .bind(user)
            .execute(&pool)
            .await?;
        let (status, error) = call("PATCH", patch_path.clone(), valid_patch.clone()).await?;
        anyhow::ensure!(status == StatusCode::FORBIDDEN, "viewer patch: {status} {error}");
        anyhow::ensure!(saved_rule().await? == before, "permission rejection must not write the rule");
        sqlx::query("UPDATE kb_members SET role = 'editor' WHERE kb_id = $1 AND user_id = $2")
            .bind(kb)
            .bind(user)
            .execute(&pool)
            .await?;
        let (status, updated) = call("PATCH", patch_path, valid_patch).await?;
        anyhow::ensure!(status == StatusCode::OK, "valid patch: {status} {updated}");
        anyhow::ensure!(updated["ok"] == true, "valid patch response: {updated}");
        let after = saved_rule().await?;
        anyhow::ensure!(after["rule"]["conclusion"] == "attribute", "new conclusion: {after}");
        anyhow::ensure!(after["rule"]["join_predicate_id"].is_null(), "the valid patch clears the join: {after}");
        anyhow::ensure!(after["conditions"][0]["subject_side"] == "x", "new conditions: {after}");
        let versions = after["versions"].as_array().ok_or_else(|| anyhow::anyhow!("missing rule versions: {after}"))?;
        anyhow::ensure!(versions.len() == 2, "only the successful definition patch opens a version: {versions:?}");
        let (status, report) = call("POST", run_path, json!({})).await?;
        anyhow::ensure!(status == StatusCode::OK, "run after valid patch: {status} {report}");
        anyhow::ensure!(report["hits"] == 1, "X.pressure = 120 matches: {report}");
        let flags: Vec<(Uuid, Value)> = sqlx::query_as(
            "SELECT subject_id, object_value FROM derived_facts
             WHERE kb_id = $1 AND predicate_id = $2 AND invalidated_at IS NULL",
        )
        .bind(kb)
        .bind(flag)
        .fetch_all(&pool)
        .await?;
        anyhow::ensure!(flags == vec![(x, json!({ "value": true }))], "the valid patch derives only X.flag = true: {flags:?}");
        anyhow::Ok(())
    }
    .await;
    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(org)
        .execute(&pool)
        .await?;
    result
}
