//! Common Connect transport configuration, independent of the guest listener.
use connectrpc::interceptor::{NextStream, PayloadStream, StreamRequest, StreamResponse};
use connectrpc::{ConnectError, ConnectRpcService, DeadlinePolicy, Interceptor, Protocol, Router};

mod input;
mod legacy;
mod protojson_schema;
pub(crate) use input::{encode_input_response, input_response_encoding, observe_request};
pub(crate) use protojson_schema::ensure_registered as ensure_protojson_schema;

struct ProtocolVersion;

#[connectrpc::async_trait]
impl Interceptor for ProtocolVersion {
    async fn intercept_streaming(
        &self,
        request: StreamRequest,
        inbound: PayloadStream,
        next: NextStream<'_>,
    ) -> Result<StreamResponse, ConnectError> {
        if request.ctx.protocol() == Some(Protocol::Connect) {
            if let Some(version) = request.ctx.header("connect-protocol-version") {
                if !version.as_bytes().is_empty() && version.as_bytes() != b"1" {
                    return Err(ConnectError::invalid_argument(format!(
                        "Connect-Protocol-Version must be \"1\": got {:?}",
                        String::from_utf8_lossy(version.as_bytes())
                    )));
                }
            }
        }
        next.run(request, inbound).await
    }
}

/// Bound response subscriptions by the negotiated RPC deadline after headers.
/// Process ownership is separate: dropping a subscription must not kill a child.
pub fn rpc_service(router: Router) -> ConnectRpcService {
    ConnectRpcService::new(router)
        .with_limits(connectrpc::Limits::unlimited())
        .with_compression(connectrpc::CompressionRegistry::default().register(input::GoGzip))
        .with_compression_policy(connectrpc::CompressionPolicy::default().with_min_size(0))
        .with_interceptor(ProtocolVersion)
        .with_interceptor(legacy::LegacyFilesystem)
        .with_interceptor(input::InputErrors)
        .with_deadline_policy(DeadlinePolicy::new().with_enforce_on_streams(true))
}
