//! Exact connect-python compatibility from the pinned Go filesystem interceptor.
use connectrpc::{
    interceptor::{
        Next, NextStream, PayloadStream, StreamRequest, StreamResponse, UnaryRequest, UnaryResponse,
    },
    ConnectError, Interceptor, Payload, RequestContext,
};
use futures_util::StreamExt;

use crate::proto::{filesystem as pb, legacyfilesystem as old};

pub(super) struct LegacyFilesystem;
const NOTIFY: &str = "x-e2b-legacy-sdk";

fn selected(ctx: &RequestContext) -> bool {
    ctx.path()
        .is_some_and(|path| path.starts_with("/filesystem.Filesystem/"))
        && ctx
            .header("user-agent")
            .is_some_and(|agent| agent == "connect-python")
}

#[connectrpc::async_trait]
impl Interceptor for LegacyFilesystem {
    async fn intercept_unary(
        &self,
        request: UnaryRequest,
        next: Next<'_>,
    ) -> Result<UnaryResponse, ConnectError> {
        let legacy = selected(&request.ctx);
        let path = request.ctx.path().unwrap_or("").to_owned();
        let mut response = next.run(request).await?;
        if legacy {
            response.body = convert(&path, response.body)?;
            response.headers.insert(
                NOTIFY,
                "true"
                    .parse()
                    .map_err(|_| ConnectError::internal("invalid legacy header"))?,
            );
        }
        Ok(response)
    }

    async fn intercept_streaming(
        &self,
        request: StreamRequest,
        inbound: PayloadStream,
        next: NextStream<'_>,
    ) -> Result<StreamResponse, ConnectError> {
        if !selected(&request.ctx) {
            return next.run(request, inbound).await;
        }
        let path = request.ctx.path().unwrap_or("").to_owned();
        let notify = axum::http::HeaderValue::from_static("true");
        // Go sets this before invoking a streaming handler, including handler errors.
        let mut response = next.run(request, inbound).await.map_err(|mut error| {
            error.response_headers_mut().insert(NOTIFY, notify.clone());
            error
        })?;
        response.headers.insert(NOTIFY, notify);
        Ok(response.map_body(move |stream| -> PayloadStream {
            Box::pin(stream.map(move |item| item.and_then(|payload| convert(&path, payload))))
        }))
    }
}

fn entry(value: &pb::EntryInfo) -> old::EntryInfo {
    old::EntryInfo {
        name: value.name.clone(),
        r#type: value.r#type.to_i32().into(),
        path: value.path.clone(),
        ..Default::default()
    }
}

fn event(value: &pb::FilesystemEvent) -> old::FilesystemEvent {
    old::FilesystemEvent {
        name: value.name.clone(),
        r#type: value.r#type.to_i32().into(),
        ..Default::default()
    }
}

/// Decode a response the guest itself just encoded.
///
/// Go's interceptor converts the handler's typed message, so it never runs
/// protojson over its own output. `Payload::message` would, and Go's
/// protojson limits a document to 10,000 decoded messages — a `ListDir` of a
/// large directory exceeds that. Trusted internal output therefore uses plain
/// serde for JSON and `Payload::message` for protobuf.
fn decode<M>(payload: &Payload) -> Result<M, ConnectError>
where
    M: buffa::Message
        + buffa::MessageName
        + serde::Serialize
        + serde::de::DeserializeOwned
        + Clone
        + 'static,
{
    match payload.format() {
        connectrpc::CodecFormat::Json => serde_json::from_slice(payload.bytes())
            .map_err(|error| ConnectError::internal(format!("legacy response decode: {error}"))),
        _ => Ok(payload.message::<M>()?.clone()),
    }
}

fn convert(path: &str, payload: Payload) -> Result<Payload, ConnectError> {
    let format = payload.format();
    macro_rules! entry_response {
        ($modern:ty, $legacy:ident) => {{
            let value = decode::<$modern>(&payload)?;
            let converted = old::$legacy {
                entry: value
                    .entry
                    .as_option()
                    .map(entry)
                    .map(Into::into)
                    .unwrap_or_default(),
                ..Default::default()
            };
            Payload::from_message(converted, format)
        }};
    }
    Ok(match path.rsplit('/').next().unwrap_or("") {
        "Stat" => entry_response!(pb::StatResponse, StatResponse),
        "Move" => entry_response!(pb::MoveResponse, MoveResponse),
        "MakeDir" => entry_response!(pb::MakeDirResponse, MakeDirResponse),
        "ListDir" => Payload::from_message(
            old::ListDirResponse {
                entries: decode::<pb::ListDirResponse>(&payload)?
                    .entries
                    .iter()
                    .map(entry)
                    .collect(),
                ..Default::default()
            },
            format,
        ),
        "Remove" => Payload::from_message(old::RemoveResponse::default(), format),
        "CreateWatcher" => Payload::from_message(
            old::CreateWatcherResponse {
                watcher_id: decode::<pb::CreateWatcherResponse>(&payload)?
                    .watcher_id
                    .clone(),
                ..Default::default()
            },
            format,
        ),
        "GetWatcherEvents" => Payload::from_message(
            old::GetWatcherEventsResponse {
                events: decode::<pb::GetWatcherEventsResponse>(&payload)?
                    .events
                    .iter()
                    .map(event)
                    .collect(),
                ..Default::default()
            },
            format,
        ),
        "WatchDir" => {
            let value = decode::<pb::WatchDirResponse>(&payload)?;
            Payload::from_message(watch(&value), format)
        }
        // Go has no converter for RemoveWatcher, but still sends the notification.
        _ => payload,
    })
}

fn watch(value: &pb::WatchDirResponse) -> old::WatchDirResponse {
    use old::watch_dir_response as legacy;
    use pb::watch_dir_response::Event;
    let event = value.event.as_ref().map(|value| match value {
        Event::Start(_) => legacy::Event::Start(Box::default()),
        Event::Keepalive(_) => legacy::Event::Keepalive(Box::default()),
        Event::Filesystem(value) => legacy::Event::Filesystem(Box::new(event(value))),
    });
    old::WatchDirResponse {
        event,
        ..Default::default()
    }
}
