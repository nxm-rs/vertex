//! Per-request gRPC observability: a tower layer that opens a span and records
//! request, latency, and outcome metrics for every method, keyed by the request
//! path and the terminal gRPC status code.
//!
//! The outcome is read from the response's `grpc-status`, which tonic emits
//! either in the leading headers (a trailers-only error) or in the trailing
//! header frame (the success and streaming case); both are covered so a metric
//! is recorded exactly once, and a stream dropped before its trailers is
//! attributed to `cancelled`.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use bytes::Bytes;
use http::{HeaderMap, Request, Response};
use http_body::{Body, Frame, SizeHint};
use metrics::{counter, histogram};
use pin_project_lite::pin_project;
use tonic::Code;
use tonic::body::BoxBody;
use tower_layer::Layer;
use tracing::{Span, info_span};
use vertex_metrics::HistogramBucketConfig;
use vertex_util_runtime::time::Instant;

use tonic::codegen::Service;

/// Histogram buckets for the gRPC request-duration family (network-latency preset).
pub const HISTOGRAM_BUCKETS: &[HistogramBucketConfig] = &[HistogramBucketConfig {
    suffix: "grpc_request_duration_seconds",
    buckets: vertex_metrics::buckets::DURATION_NETWORK,
}];

/// Header carrying the terminal gRPC status code.
const GRPC_STATUS: &str = "grpc-status";

/// Tower layer wrapping the gRPC router with per-request observability.
#[derive(Clone, Copy, Debug, Default)]
pub struct GrpcObserveLayer;

impl GrpcObserveLayer {
    /// A new observability layer.
    pub const fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for GrpcObserveLayer {
    type Service = GrpcObserve<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcObserve { inner }
    }
}

/// Service produced by [`GrpcObserveLayer`].
#[derive(Clone, Debug)]
pub struct GrpcObserve<S> {
    inner: S,
}

impl<S, ReqBody> Service<Request<ReqBody>> for GrpcObserve<S>
where
    S: Service<Request<ReqBody>, Response = Response<BoxBody>>,
{
    type Response = Response<BoxBody>;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let method = method_label(req.uri().path());
        counter!("grpc_requests_total", "method" => method.clone()).increment(1);
        let span = info_span!("grpc_request", method = %method);
        let record = RequestRecord {
            method,
            start: Instant::now(),
            done: false,
        };
        ResponseFuture {
            inner: self.inner.call(req),
            record: Some(record),
            span,
        }
    }
}

pin_project! {
    /// Future that records the outcome once the response head resolves, wrapping
    /// the body when the status is only known at end of stream.
    pub struct ResponseFuture<F> {
        #[pin]
        inner: F,
        record: Option<RequestRecord>,
        span: Span,
    }
}

impl<F, E> Future for ResponseFuture<F>
where
    F: Future<Output = Result<Response<BoxBody>, E>>,
{
    type Output = Result<Response<BoxBody>, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _enter = this.span.enter();
        let resp = ready!(this.inner.poll(cx))?;

        // A trailers-only error carries `grpc-status` in the leading headers, so
        // the outcome is known without draining the body.
        if let Some(code) = status_from_headers(resp.headers())
            && let Some(mut record) = this.record.take()
        {
            record.finish(code);
            return Poll::Ready(Ok(resp));
        }

        // Otherwise the status rides the trailing header frame: observe the body.
        let record = this.record.take();
        Poll::Ready(Ok(resp.map(|body| {
            tonic::body::boxed(ObservedBody {
                inner: body,
                record,
            })
        })))
    }
}

pin_project! {
    /// Response body that records the request outcome from its trailing status.
    ///
    /// Records `cancelled` on drop if no terminal frame was observed, so a stream
    /// abandoned mid-flight still resolves an outcome.
    struct ObservedBody {
        #[pin]
        inner: BoxBody,
        record: Option<RequestRecord>,
    }
}

impl Body for ObservedBody {
    type Data = Bytes;
    type Error = tonic::Status;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        let frame = ready!(this.inner.poll_frame(cx));
        match &frame {
            Some(Ok(f)) => {
                if let Some(trailers) = f.trailers_ref()
                    && let Some(record) = this.record.as_mut()
                {
                    record.finish(status_from_headers(trailers).unwrap_or(Code::Ok));
                }
            }
            Some(Err(status)) => {
                if let Some(record) = this.record.as_mut() {
                    record.finish(status.code());
                }
            }
            None => {
                if let Some(record) = this.record.as_mut() {
                    record.finish(Code::Ok);
                }
            }
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Per-request timing and outcome recorder. Emits the duration histogram and the
/// outcome counter exactly once.
struct RequestRecord {
    method: String,
    start: Instant,
    done: bool,
}

impl RequestRecord {
    fn finish(&mut self, code: Code) {
        if self.done {
            return;
        }
        self.done = true;
        histogram!("grpc_request_duration_seconds", "method" => self.method.clone())
            .record(self.start.elapsed().as_secs_f64());
        counter!(
            "grpc_request_outcomes_total",
            "method" => self.method.clone(),
            "code" => code_label(code),
        )
        .increment(1);
    }
}

impl Drop for RequestRecord {
    fn drop(&mut self) {
        self.finish(Code::Cancelled);
    }
}

/// The request path without its leading slash, or `unknown` when absent.
fn method_label(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Parse the `grpc-status` code from a header map, if present.
fn status_from_headers(headers: &HeaderMap) -> Option<Code> {
    let raw = headers.get(GRPC_STATUS)?;
    let code = raw.to_str().ok()?.parse::<i32>().ok()?;
    Some(Code::from(code))
}

/// Bounded snake_case label for a gRPC status code.
fn code_label(code: Code) -> &'static str {
    match code {
        Code::Ok => "ok",
        Code::Cancelled => "cancelled",
        Code::Unknown => "unknown",
        Code::InvalidArgument => "invalid_argument",
        Code::DeadlineExceeded => "deadline_exceeded",
        Code::NotFound => "not_found",
        Code::AlreadyExists => "already_exists",
        Code::PermissionDenied => "permission_denied",
        Code::ResourceExhausted => "resource_exhausted",
        Code::FailedPrecondition => "failed_precondition",
        Code::Aborted => "aborted",
        Code::OutOfRange => "out_of_range",
        Code::Unimplemented => "unimplemented",
        Code::Internal => "internal",
        Code::Unavailable => "unavailable",
        Code::DataLoss => "data_loss",
        Code::Unauthenticated => "unauthenticated",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_label_trims_leading_slash() {
        assert_eq!(
            method_label("/vertex.swarm.node.v1.Node/GetStatus"),
            "vertex.swarm.node.v1.Node/GetStatus"
        );
        assert_eq!(method_label("/"), "unknown");
        assert_eq!(method_label(""), "unknown");
    }

    #[test]
    fn status_from_headers_reads_grpc_status() {
        let mut headers = HeaderMap::new();
        headers.insert(GRPC_STATUS, "5".parse().unwrap());
        assert_eq!(status_from_headers(&headers), Some(Code::NotFound));

        assert_eq!(status_from_headers(&HeaderMap::new()), None);
    }

    #[test]
    fn code_label_is_snake_case() {
        assert_eq!(code_label(Code::Ok), "ok");
        assert_eq!(code_label(Code::InvalidArgument), "invalid_argument");
        assert_eq!(code_label(Code::Unavailable), "unavailable");
    }
}
