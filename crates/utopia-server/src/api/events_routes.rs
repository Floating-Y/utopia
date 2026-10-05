//! KB 事件流（SSE）：文档摄入/抽取状态与审核队列变化的实时推送。
//! 前端收到事件只做 react-query 失效重取——事件本身不带业务数据，天然幂等。
//!
//! **告警也从这条流走**（#1028）。浏览器给一个源的 HTTP/1.1 连接只有六条，事件流占着
//! 不放：一页开两条通知流、再加一条正在生成的回答，两个标签页就占满，Stop 发不出去。
//! 所以打开着一个库的页只订这一条，全局那条 `/alerts/events` 留给没有库的页。

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use std::convert::Infallible;
use tokio::sync::broadcast;
use utopia_core::models::Role;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::{AppEvent, AppState};

pub async fn kb_events(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    utopia_store::access::require_kb(&state.pool, &user, kb_id, Role::Viewer).await?;

    Ok(kb_event_stream(state.events.subscribe(), kb_id))
}

fn kb_event_stream(
    mut rx: broadcast::Receiver<AppEvent>,
    kb_id: Uuid,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(ev) => match relay(&ev, kb_id) {
                    Some(Relay::Alert) => yield Ok(Event::default().event("alert").data("{}")),
                    Some(Relay::Kb) => yield Ok(Event::default()
                        .event(ev.kind)
                        .data(serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into()))),
                    None => continue,
                },
                // 丢掉的可能是这个库或告警的唯一通知；余下事件可能全部被过滤。
                // 结束响应，让 EventSource 重连后通过 onRecover 补刷。
                Err(broadcast::error::RecvError::Lagged(_)) => return,
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Debug, PartialEq)]
enum Relay {
    /// 告警：不按库过滤、不带数据，和 `alerts_routes::stream` 同一个约定——收到的人
    /// 回头重取列表，谁能看见什么由列表查询说了算
    Alert,
    /// 这个库自己的事件，原样送出
    Kb,
}

fn relay(ev: &AppEvent, kb_id: Uuid) -> Option<Relay> {
    if ev.kind == "alert" {
        Some(Relay::Alert)
    } else if ev.kb_id == Some(kb_id) {
        Some(Relay::Kb)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use axum::{routing::get, Router};
    use futures_util::{FutureExt, StreamExt};
    use std::time::Duration;

    #[tokio::test]
    async fn lagged_kb_stream_ends_when_its_only_document_or_global_alert_is_lost() {
        let (here, elsewhere) = (Uuid::now_v7(), Uuid::now_v7());
        for (kb_id, kind) in [(Some(here), "document"), (None, "alert")] {
            let (sender, _) = broadcast::channel(256);
            let route_sender = sender.clone();
            let app = Router::new().route(
                "/events",
                get(move || {
                    let sender = route_sender.clone();
                    async move {
                        let receiver = sender.subscribe();
                        sender
                            .send(AppEvent {
                                kb_id,
                                kind,
                                document_id: None,
                            })
                            .unwrap();
                        // 首次 poll 前确定性地挤掉唯一有效通知，剩下的全部会被过滤。
                        for _ in 0..257 {
                            sender
                                .send(AppEvent {
                                    kb_id: Some(elsewhere),
                                    kind: "document",
                                    document_id: None,
                                })
                                .unwrap();
                        }
                        kb_event_stream(receiver, here)
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let request = async {
                let response = client
                    .get(format!("http://{address}/events"))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), reqwest::StatusCode::OK);
                assert_eq!(response.headers()["content-type"], "text/event-stream");
                response.text().await.unwrap()
            };
            // 保留 sender，确保真实 HTTP 响应结束来自 Lagged，而不是通道关闭。
            let result = tokio::time::timeout(Duration::from_secs(2), request).await;
            server.abort();
            assert!(result
                .expect("a lagged KB SSE response must end")
                .is_empty());
        }
    }

    #[tokio::test]
    async fn kb_stream_keeps_filtering_and_alert_payloads_until_the_channel_closes() {
        let (here, elsewhere) = (Uuid::now_v7(), Uuid::now_v7());
        let (sender, receiver) = broadcast::channel(256);
        let response = kb_event_stream(receiver, here).into_response();
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let mut body = response.into_body().into_data_stream();
        // 初次连接不凭空发刷新事件，也不结束响应。
        assert!(body.next().now_or_never().is_none());
        sender
            .send(AppEvent {
                kb_id: Some(elsewhere),
                kind: "document",
                document_id: None,
            })
            .unwrap();
        assert!(body.next().now_or_never().is_none());

        let document = AppEvent {
            kb_id: Some(here),
            kind: "document",
            document_id: Some(Uuid::now_v7()),
        };
        sender.send(document.clone()).unwrap();
        let frame = body.next().await.unwrap().unwrap();
        assert_eq!(
            frame.as_ref(),
            format!(
                "event: document\ndata: {}\n\n",
                serde_json::to_string(&document).unwrap()
            )
            .as_bytes()
        );

        for kb_id in [None, Some(here), Some(elsewhere)] {
            sender
                .send(AppEvent {
                    kb_id,
                    kind: "alert",
                    document_id: Some(Uuid::now_v7()),
                })
                .unwrap();
            let frame = body.next().await.unwrap().unwrap();
            assert_eq!(frame.as_ref(), b"event: alert\ndata: {}\n\n");
        }
        assert!(body.next().now_or_never().is_none());
        drop(sender);
        assert!(body.next().await.is_none());
    }

    #[tokio::test]
    async fn dropping_a_kb_response_releases_its_subscription() {
        let (sender, receiver) = broadcast::channel(256);
        let response = kb_event_stream(receiver, Uuid::now_v7()).into_response();
        assert_eq!(sender.receiver_count(), 1);
        drop(response);
        assert_eq!(sender.receiver_count(), 0);
    }

    #[test]
    fn a_kb_stream_carries_its_own_events_and_every_alert() {
        let (here, elsewhere) = (Uuid::now_v7(), Uuid::now_v7());
        let event = |kb_id, kind| AppEvent {
            kb_id,
            kind,
            document_id: None,
        };
        assert_eq!(relay(&event(Some(here), "document"), here), Some(Relay::Kb));
        assert_eq!(relay(&event(Some(elsewhere), "document"), here), None);
        // 系统级告警没有库，别的库的告警也要叫醒角标：都送，都不带数据
        for kb_id in [None, Some(here), Some(elsewhere)] {
            assert_eq!(relay(&event(kb_id, "alert"), here), Some(Relay::Alert));
        }
    }
}
