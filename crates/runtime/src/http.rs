//! Serving HTTP requests with `wasi:http/proxy` components.

use std::error::Error as StdError;
use std::future::Future;

use http_body_util::BodyExt;
use hyper::body::{Body, Bytes};
use tokio::sync::oneshot;
use wasmtime::{Result, Store, bail};
use wasmtime_wasi::WasiCtx;
use wasmtime_wasi_http::p2::bindings::ProxyPre;
use wasmtime_wasi_http::p2::bindings::http::types::Scheme;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::{
    Error as HttpError, RequestOptions, WasiBody, WasiHttpHooks, WasiHttpView,
};

use crate::State;

/// Body of a response produced by an [`HttpFunction`].
pub type ResponseBody = HyperOutgoingBody;

/// A compiled HTTP function, ready to handle requests.
///
/// Cheap to clone. Every request runs in a fresh, isolated instance.
#[derive(Clone)]
pub struct HttpFunction {
    pre: ProxyPre<State>,
}

impl HttpFunction {
    pub(crate) fn new(pre: ProxyPre<State>) -> Self {
        Self { pre }
    }

    /// Runs the function on `req` and returns its response.
    ///
    /// Must be called from within a Tokio runtime.
    pub async fn handle<B>(&self, req: hyper::Request<B>) -> Result<hyper::Response<ResponseBody>>
    where
        B: Body<Data = Bytes> + Send + 'static,
        B::Error: Into<Box<dyn StdError + Send + Sync>>,
    {
        let req = req
            .map(|body| body.map_err(|err| HttpError::InternalError(Some(err.into().to_string()))));

        let wasi = WasiCtx::builder().inherit_stdout().inherit_stderr().build();
        let mut store = Store::new(self.pre.engine(), State::new(wasi));

        let (sender, receiver) = oneshot::channel();
        let req = store
            .data_mut()
            .http()
            .new_incoming_request(Scheme::Http, req)?;
        let out = store.data_mut().http().new_response_outparam(sender)?;

        // Run the guest in its own task: it can keep streaming the body after
        // the response headers have been sent.
        let pre = self.pre.clone();
        let task = tokio::spawn(async move {
            let proxy = pre.instantiate_async(&mut store).await?;
            proxy
                .wasi_http_incoming_handler()
                .call_handle(&mut store, req, out)
                .await
        });

        match receiver.await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(code)) => bail!("function returned an error: {code:?}"),
            // The guest dropped the response handle without using it.
            Err(_) => match task.await {
                Ok(Ok(())) => bail!("function finished without sending a response"),
                Ok(Err(err)) => Err(err.context("function trapped")),
                Err(err) => Err(wasmtime::Error::from(err).context("function task panicked")),
            },
        }
    }
}

type OutgoingIoFuture = Box<dyn Future<Output = Result<(), HttpError>> + Send>;

/// Rejects every HTTP request a guest tries to send.
///
/// Outbound access will be opt-in, per function.
pub(crate) struct DenyOutbound;

impl WasiHttpHooks for DenyOutbound {
    fn send_request(
        &mut self,
        _request: hyper::Request<WasiBody>,
        _options: Option<RequestOptions>,
        _fut: OutgoingIoFuture,
    ) -> Box<
        dyn Future<Output = Result<(hyper::Response<WasiBody>, OutgoingIoFuture), HttpError>>
            + Send,
    > {
        Box::new(async { Err(HttpError::HttpRequestDenied) })
    }
}
