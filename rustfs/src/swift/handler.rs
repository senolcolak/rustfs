// Copyright 2024 RustFS Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Swift HTTP handler
//!
//! This module provides the HTTP request handler that routes Swift API
//! requests and delegates to appropriate Swift handlers or falls through
//! to S3 service for non-Swift requests.

use crate::swift::{SwiftRoute, SwiftRouter};
use axum::http::{Request, Response, StatusCode};
use futures::Future;
use s3s::Body;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::Service;
use tracing::{debug, instrument};

/// Swift-aware service that routes to Swift handlers or S3 service
#[derive(Clone)]
pub struct SwiftService<S> {
    /// Swift router for URL parsing
    router: SwiftRouter,
    /// Underlying S3 service for fallback
    s3_service: S,
}

impl<S> SwiftService<S> {
    /// Create a new Swift service wrapping an S3 service
    pub fn new(enabled: bool, url_prefix: Option<String>, s3_service: S) -> Self {
        let router = SwiftRouter::new(enabled, url_prefix);
        Self { router, s3_service }
    }
}

impl<S, B> Service<Request<B>> for SwiftService<S>
where
    S: Service<Request<B>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    B: Send + 'static,
{
    type Response = Response<Body>;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.s3_service.poll_ready(cx).map_err(Into::into)
    }

    #[instrument(skip(self, req), fields(method = %req.method(), uri = %req.uri()))]
    fn call(&mut self, req: Request<B>) -> Self::Future {
        let router = self.router.clone();
        let method = req.method().clone();
        let uri = req.uri().clone();

        // Try to parse as Swift request
        if let Some(route) = router.route(&uri, method.clone()) {
            debug!("Swift route matched: {:?}", route);

            // For Phase 1, return "Not Implemented" for all Swift operations
            // This will be replaced with actual handlers in Phase 2-3
            let response = handle_swift_request_phase1(route);
            return Box::pin(async move { Ok(response) });
        }

        // Not a Swift request, delegate to S3 service
        debug!("No Swift route matched, delegating to S3 service");
        let mut s3_service = self.s3_service.clone();
        Box::pin(async move { s3_service.call(req).await.map_err(Into::into) })
    }
}

/// Phase 1 handler - returns 501 Not Implemented for all Swift operations
/// This will be replaced with actual implementations in Phase 2-3
fn handle_swift_request_phase1(route: SwiftRoute) -> Response<Body> {
    let message = match route {
        SwiftRoute::Account { account, method } => {
            format!("Swift Account operation not yet implemented: {} {}", method, account)
        }
        SwiftRoute::Container {
            account,
            container,
            method,
        } => {
            format!("Swift Container operation not yet implemented: {} {}/{}", method, account, container)
        }
        SwiftRoute::Object {
            account,
            container,
            object,
            method,
        } => {
            format!(
                "Swift Object operation not yet implemented: {} {}/{}/{}",
                method, account, container, object
            )
        }
    };

    // Generate transaction ID
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros();
    let trans_id = format!("tx{:x}", timestamp);

    Response::builder()
        .status(StatusCode::NOT_IMPLEMENTED)
        .header("content-type", "text/plain; charset=utf-8")
        .header("x-trans-id", trans_id.clone())
        .header("x-openstack-request-id", trans_id)
        .body(Body::from(message))
        .unwrap()
}

// Tests commented out for Phase 1 - will be fixed in Phase 2 when we have real handlers
// #[cfg(test)]
// mod tests {
//     // Tests will be re-enabled in Phase 2
// }
